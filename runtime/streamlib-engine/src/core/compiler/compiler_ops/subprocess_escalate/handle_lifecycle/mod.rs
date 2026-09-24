// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What the parent holds on a helper process's behalf, and the release every
//! acquire owes — by explicit `release_handle` or at bridge teardown.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::core::compiler::compiler_ops::subprocess_bridge::SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestReleaseHandle;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::{GpuContextLimitedAccess, PooledTextureHandle};
use crate::core::processor_owned_window::WindowPresentLoopForOwningProcessor;
use crate::core::rhi::PixelBuffer;

/// Resource kept alive on behalf of a subprocess by
/// [`EscalateHandleRegistry`]. The fields are only read via the `Drop`
/// side-effect that releases them back to the host pool when removed
/// from the registry — the map keeps the resource live, the resource's
/// destructor does the release on removal.
///
/// Post-#562: cpu-readback no longer registers per-acquire handles.
/// Staging buffers + timeline are pre-registered with surface-share
/// at startup and the subprocess imports them once via
/// `streamlib-consumer-rhi`; per-acquire IPC reduces to a thin
/// `run_cpu_readback_copy` trigger that returns a timeline value.
///
/// Timeline Arcs (when present) keep the per-edge single-writer
/// timelines alive for the registration's lifetime — surface-share
/// duplicates the FDs at register-time via SCM_RIGHTS, but the
/// host-side `Arc<HostVulkanTimelineSemaphore>` must outlive the
/// registration so the kernel objects backing the FDs aren't
/// destroyed. See `docs/architecture/adapter-timeline-single-writer.md`.
pub(crate) enum RegisteredHandle {
    #[allow(dead_code)]
    PixelBuffer(PixelBuffer),
    #[allow(dead_code)]
    Texture {
        texture: PooledTextureHandle,
        #[cfg(target_os = "linux")]
        produce_done: Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
        #[cfg(target_os = "linux")]
        consume_done: Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
        #[cfg(target_os = "macos")]
        timeline_pair: Option<Arc<crate::apple::surface_share::CrossProcessTimelinePair>>,
    },
    /// Render-target image handed out via `AcquireImage`. The texture
    /// itself returns to its pool when the variant drops; the
    /// timelines keep their FDs alive for surface-share consumers.
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    Image {
        texture: crate::core::rhi::Texture,
        produce_done: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    },
}

impl RegisteredHandle {
    /// Whether releasing this handle also owes the parent's texture-cache
    /// entry an eviction — textures and images enter it at acquire, pixel
    /// buffers never do.
    pub(crate) fn is_texture_backed(&self) -> bool {
        match self {
            Self::PixelBuffer(_) => false,
            Self::Texture { .. } => true,
            #[cfg(target_os = "linux")]
            Self::Image { .. } => true,
        }
    }
}

/// Tracks resources acquired on behalf of a subprocess so `release_handle` —
/// or subprocess death — can drop the host's strong reference. Resources stay
/// alive for the duration of the host pool; this map simply prevents the
/// resource from being immediately recycled while the subprocess still
/// references it by ID. Dropping a [`PooledTextureHandle`] releases the pool
/// slot; dropping an [`PixelBuffer`] releases its refcount.
///
/// It is the one per-subprocess state the escalate dispatch has, so the
/// windows that subprocess owns and the lifecycle hook it is currently inside
/// live here too — both are per-helper, and both are released or forgotten at
/// the same teardown.
#[derive(Default)]
pub(crate) struct EscalateHandleRegistry {
    handles: Mutex<HashMap<String, RegisteredHandle>>,
    /// The windows this subprocess owns, each driving its own present
    /// thread. Not a [`RegisteredHandle`]: releasing one stops and waits for
    /// a thread rather than evicting a texture-cache entry, and a window id
    /// must never reach the surface-share release path.
    ///
    /// Held by `Arc` so every op works on a window with this map's lock
    /// already released: closing one waits on a thread the window server can
    /// hold for as long as it likes, and `SubprocessBridge`'s teardown — a
    /// path that deliberately never blocks — takes this same lock.
    processor_owned_windows: Mutex<HashMap<String, Arc<WindowPresentLoopForOwningProcessor>>>,
    /// The lifecycle command the parent last sent this helper — the engine's
    /// only reading of which hook the child is inside.
    ///
    /// Setup-phase-only ops refuse on it. The helper's own typestate (which
    /// Python object carries the method) is the child's guard and never the
    /// engine's: both Python capability tiers collapse onto this one wire.
    last_lifecycle_command_sent_to_the_helper_process: Mutex<Option<String>>,
}

impl EscalateHandleRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn insert_buffer(&self, handle_id: String, buffer: PixelBuffer) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(handle_id, RegisteredHandle::PixelBuffer(buffer));
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn insert_texture(
        &self,
        handle_id: String,
        texture: PooledTextureHandle,
        produce_done: Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
        consume_done: Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
    ) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(
            handle_id,
            RegisteredHandle::Texture {
                texture,
                produce_done,
                consume_done,
            },
        );
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn insert_texture(
        &self,
        handle_id: String,
        texture: PooledTextureHandle,
        timeline_pair: Option<Arc<crate::apple::surface_share::CrossProcessTimelinePair>>,
    ) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(
            handle_id,
            RegisteredHandle::Texture {
                texture,
                timeline_pair,
            },
        );
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn insert_texture(&self, handle_id: String, texture: PooledTextureHandle) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(handle_id, RegisteredHandle::Texture { texture });
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn insert_image(
        &self,
        handle_id: String,
        texture: crate::core::rhi::Texture,
        produce_done: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(
            handle_id,
            RegisteredHandle::Image {
                texture,
                produce_done,
                consume_done,
            },
        );
    }

    /// Remove a handle by id, handing back what was held so the caller can
    /// pair its removal with the kind-specific cleanup. `None` when the id
    /// was unknown. Used by the escalate `release_handle` path.
    pub(crate) fn remove_handle(&self, handle_id: &str) -> Option<RegisteredHandle> {
        let mut map = self.handles.lock().expect("poisoned");
        map.remove(handle_id)
    }

    /// Take every held handle, ids included, so a teardown path can run the
    /// same kind-specific cleanup the explicit release path does.
    pub(crate) fn drain_handles(&self) -> Vec<(String, RegisteredHandle)> {
        let mut map = self.handles.lock().expect("poisoned");
        map.drain().collect()
    }

    /// Number of currently-held handles; visible for tests.
    #[cfg(test)]
    pub(crate) fn handle_count(&self) -> usize {
        self.handles.lock().expect("poisoned").len()
    }

    /// Record the lifecycle command the parent is about to send the helper,
    /// so setup-phase-only escalate ops can refuse everything else by name.
    pub(crate) fn note_lifecycle_command_sent_to_the_helper_process(&self, command: &str) {
        *self
            .last_lifecycle_command_sent_to_the_helper_process
            .lock()
            .expect("poisoned") = Some(command.to_string());
    }

    /// Whether the last thing the parent told this helper to do was `setup`.
    ///
    /// Named for what it reads rather than for the conclusion drawn from it:
    /// nothing clears the field when a hook returns, so this still answers
    /// true between setup completing and `run` being sent. That is the whole
    /// engine-side reading of the child's phase, and it fails closed —
    /// `false` before the parent has sent anything at all.
    pub(crate) fn the_last_lifecycle_command_sent_to_the_helper_process_was_setup(&self) -> bool {
        self.last_lifecycle_command_sent_to_the_helper_process
            .lock()
            .expect("poisoned")
            .as_deref()
            == Some(SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS)
    }

    /// The window map, recovered rather than panicked on when poisoned.
    ///
    /// The drain below runs from `SubprocessBridge`'s `Drop`, where a second
    /// panic would abort the process instead of tearing one helper down.
    fn locked_processor_owned_windows(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, Arc<WindowPresentLoopForOwningProcessor>>> {
        self.processor_owned_windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn insert_processor_owned_window(
        &self,
        window_id: String,
        present_loop: WindowPresentLoopForOwningProcessor,
    ) {
        let mut windows = self.locked_processor_owned_windows();
        windows.insert(window_id, Arc::new(present_loop));
    }

    /// One window this subprocess owns, or `None` when the id names none.
    ///
    /// Hands the window out rather than running a closure under the map's
    /// lock, so no op holds it while waiting on a present thread or reaching
    /// the surface store.
    pub(crate) fn processor_owned_window(
        &self,
        window_id: &str,
    ) -> Option<Arc<WindowPresentLoopForOwningProcessor>> {
        self.locked_processor_owned_windows()
            .get(window_id)
            .cloned()
    }

    /// Take every window this subprocess still owns, so a teardown path
    /// closes them all — the release an owner that never called
    /// `close_processor_owned_window` still gets.
    pub(crate) fn drain_processor_owned_windows(
        &self,
    ) -> Vec<(String, Arc<WindowPresentLoopForOwningProcessor>)> {
        let mut windows = self.locked_processor_owned_windows();
        windows.drain().collect()
    }
}

/// Release one handle a helper process holds — a registry entry, or an
/// acceleration structure registered against `GpuContext`.
pub(super) fn handle_release_handle(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    rid: String,
    request: EscalateRequestReleaseHandle,
) -> EscalateResponse {
    let EscalateRequestReleaseHandle {
        request_id: _,
        handle_id,
    } = request;
    let removed_handle = registry.remove_handle(&handle_id);
    let removed = removed_handle.is_some();
    if let Some(removed_handle) = removed_handle {
        release_surface_share_and_texture_cache_for_handle(sandbox, &handle_id, &removed_handle);
    }
    // An acceleration structure is registered against `GpuContext`
    // rather than against the per-subprocess handle registry, so its id
    // reaches the same release verb through the device gate.
    let released = removed || release_acceleration_structure(sandbox, &handle_id);
    if released {
        EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id,
            ..Default::default()
        })
    } else {
        EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("handle_id '{handle_id}' not found in registry"),
        })
    }
}

/// Drop `GpuContext`'s strong reference to an acceleration structure, answering
/// whether the id named one.
///
/// A structure the caller built and then let go of is the only escalate-minted
/// resource whose device memory is proportional to what the caller supplied, so
/// it is the one a long-running helper must be able to hand back. Off Linux
/// nothing can have built one, so nothing can be released.
pub(super) fn release_acceleration_structure(
    sandbox: &GpuContextLimitedAccess,
    handle_id: &str,
) -> bool {
    #[cfg(target_os = "linux")]
    {
        sandbox
            .escalate(|full| Ok(full.release_acceleration_structure(handle_id)))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (sandbox, handle_id);
        false
    }
}

/// The cleanup a registry eviction owes outside the registry, shared by the
/// explicit `release_handle` op and bridge teardown so a crashed helper's
/// acquires release exactly what an explicit release would (#1901).
pub(crate) fn release_surface_share_and_texture_cache_for_handle(
    sandbox: &GpuContextLimitedAccess,
    handle_id: &str,
    removed_handle: &RegisteredHandle,
) {
    release_surface_share_surface(sandbox, handle_id, removed_handle);
    // Texture and image acquires also entered the parent's same-process
    // texture cache; `unregister_texture` removes that entry and tears
    // down the surface's export stagings with it. Scoped to
    // texture-backed handles so a buffer release keeps its staging
    // lifetime unchanged.
    if removed_handle.is_texture_backed() {
        sandbox.unregister_texture(handle_id);
    }
}

/// Best-effort surface-share release paired with registry eviction, for the
/// handles registered under their own id: every acquire on Linux, where each
/// is checked in; a texture on macOS, where a pixel buffer's id names its
/// pool slot's registration, which outlives the handle.
///
/// The registry drop alone releases the host's strong refcount on the
/// underlying resource, but the surface-share service still holds the
/// surface's handle until `release`. Errors are logged, not returned — the
/// subprocess is not waiting on the service at this point.
#[allow(unused_variables)]
pub(super) fn release_surface_share_surface(
    sandbox: &GpuContextLimitedAccess,
    handle_id: &str,
    removed_handle: &RegisteredHandle,
) {
    let registered_under_its_own_id =
        cfg!(target_os = "linux") || removed_handle.is_texture_backed();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if registered_under_its_own_id
        && let Some(store) = sandbox.surface_store()
        && let Err(e) = store.release(handle_id)
    {
        tracing::debug!(
            "[escalate] surface-share service release for '{}' returned error: {}",
            handle_id,
            e
        );
    }
}
