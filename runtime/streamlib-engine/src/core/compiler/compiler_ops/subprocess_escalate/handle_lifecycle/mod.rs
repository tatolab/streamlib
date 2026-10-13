// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What the parent holds on a helper process's behalf, and the release every
//! acquire owes — by explicit `release_handle` or at bridge teardown.

mod one_off_acquisitions_held_for_one_node;
mod processor_output_pools_of_one_helper;
#[cfg(test)]
mod tests;

pub(crate) use one_off_acquisitions_held_for_one_node::OneOffAcquisitionShape;
use one_off_acquisitions_held_for_one_node::{
    OneOffAcquisitionCountReservation, OneOffAcquisitionsHeldForOneNode,
};
pub(crate) use processor_output_pools_of_one_helper::{
    ProcessorOutputFrameHandOff, ProcessorOutputFreshSlotHandOff, ProcessorOutputPoolsOfOneHelper,
    ProcessorOutputSlotDescriptor, ProcessorOutputTextureDescriptor,
    ReleasedProcessorOutputPoolSlot,
};

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::core::compiler::compiler_ops::subprocess_bridge::SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestReleaseHandle;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::{GpuContextFullAccess, GpuContextLimitedAccess, PooledTextureHandle};
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
    /// Tensor storage buffer handed out via `AcquireStorageBuffer`, registered
    /// with surface-share and the parent-wide storage buffer map.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    StorageBuffer {
        buffer: crate::core::rhi::StorageBuffer,
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
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::StorageBuffer { .. } => false,
        }
    }

    /// Free this handle's shared pool entry as soon as nothing holds it,
    /// without waiting out the idle window — for a helper torn down.
    pub(crate) fn flag_its_shared_pool_entry_freed_as_soon_as_unheld(
        &self,
        sandbox: &GpuContextLimitedAccess,
    ) {
        match self {
            Self::Texture { texture, .. } => texture.flag_its_pool_slot_freed_as_soon_as_unheld(),
            Self::PixelBuffer(buffer) => sandbox
                .host_inner()
                .flag_the_pixel_buffer_pool_slot_of_the_buffer_freed_as_soon_as_unheld(buffer),
            // Neither is drawn from a shared pool; each is freed when its
            // last holder drops it.
            #[cfg(target_os = "linux")]
            Self::Image { .. } => {}
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::StorageBuffer { .. } => {}
        }
    }

    /// Whether releasing this handle also owes the parent-wide storage buffer
    /// map an eviction.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn is_storage_buffer_backed(&self) -> bool {
        match self {
            Self::PixelBuffer(_) | Self::Texture { .. } => false,
            #[cfg(target_os = "linux")]
            Self::Image { .. } => false,
            Self::StorageBuffer { .. } => true,
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
/// It counts the node's one-offs per shape, refusing the next by name once
/// 16 of a shape are held.
///
/// It is the one per-subprocess state the escalate dispatch has, so the
/// windows that subprocess owns and the lifecycle hook it is currently inside
/// live here too — both are per-helper, and both are released or forgotten at
/// the same teardown.
#[derive(Default)]
pub(crate) struct EscalateHandleRegistry {
    /// Taken by a reservation's drop and by teardown, so it recovers rather
    /// than panics when poisoned, and is never held across an allocation.
    one_off_acquisitions_held_for_the_helper:
        Mutex<OneOffAcquisitionsHeldForOneNode<RegisteredHandle>>,
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
    /// The processor output pools this helper's rings publish from. Their
    /// slots are never registry entries of their own: a frame's id names a
    /// generation, and the slot outlives it.
    processor_output_pools: Mutex<ProcessorOutputPoolsOfOneHelper>,
}

impl EscalateHandleRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Acquire a one-off for the helper, counted against the node's one-offs
    /// of `shape` before `allocate` runs inside the escalate, and held under
    /// the id `allocate` answers until the helper releases it; refused by name
    /// once the node holds or has reserved as many of `shape` as one node may.
    pub(super) fn acquire_a_one_off_for_the_helper_counted_under_its_shape(
        &self,
        sandbox: &GpuContextLimitedAccess,
        shape: OneOffAcquisitionShape,
        allocate: impl FnOnce(
            &GpuContextFullAccess,
        ) -> crate::core::error::Result<(String, RegisteredHandle)>,
    ) -> crate::core::error::Result<String> {
        let reservation = self.reserve_one_off_acquisition_count(shape)?;
        sandbox.escalate(move |full| {
            let (handle_id, registered_handle) = allocate(full)?;
            // Held inside the escalate: its trailing device-idle wait can fail
            // after `allocate` registered the handle, which would drop it unheld.
            reservation.hold_until_the_helper_releases_it(handle_id.clone(), registered_handle);
            Ok(handle_id)
        })
    }

    /// Reserve one count of `shape` for a one-off acquisition about to be
    /// allocated, refused by name when the node already holds or has reserved
    /// as many of that shape as one node may. The registry's lock is released
    /// before this returns.
    fn reserve_one_off_acquisition_count(
        &self,
        shape: OneOffAcquisitionShape,
    ) -> crate::core::error::Result<OneOffAcquisitionCountReservation<'_>> {
        self.locked_one_off_acquisitions_held_for_the_helper()
            .reserve_one_more_of_shape(&shape)?;
        Ok(OneOffAcquisitionCountReservation::of_a_count_the_registry_counted(self, shape))
    }

    /// The one-off acquisitions this helper holds, recovered rather than
    /// panicked on when poisoned.
    fn locked_one_off_acquisitions_held_for_the_helper(
        &self,
    ) -> MutexGuard<'_, OneOffAcquisitionsHeldForOneNode<RegisteredHandle>> {
        self.one_off_acquisitions_held_for_the_helper
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The processor output pools this helper publishes from, recovered
    /// rather than panicked on when poisoned: teardown drains them.
    pub(crate) fn processor_output_pools(&self) -> MutexGuard<'_, ProcessorOutputPoolsOfOneHelper> {
        self.processor_output_pools
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Remove a handle by id, handing back what was held so the caller can
    /// pair its removal with the kind-specific cleanup, and its count back to
    /// the node. `None` when the id was unknown. Used by the escalate
    /// `release_handle` path.
    pub(crate) fn remove_handle(&self, handle_id: &str) -> Option<RegisteredHandle> {
        self.locked_one_off_acquisitions_held_for_the_helper()
            .release_held_acquisition(handle_id)
    }

    /// Take every held handle, ids included, so a teardown path can run the
    /// same kind-specific cleanup the explicit release path does; every count
    /// they held goes back to the node.
    pub(crate) fn drain_handles(&self) -> Vec<(String, RegisteredHandle)> {
        self.locked_one_off_acquisitions_held_for_the_helper()
            .release_every_held_acquisition()
    }

    /// Number of currently-held handles; visible for tests.
    #[cfg(test)]
    pub(crate) fn handle_count(&self) -> usize {
        self.locked_one_off_acquisitions_held_for_the_helper()
            .held_count()
    }

    /// How many one-offs of `shape` this helper holds or has reserved.
    #[cfg(test)]
    pub(crate) fn one_off_acquisitions_held_or_reserved_of_shape(
        &self,
        shape: &OneOffAcquisitionShape,
    ) -> usize {
        self.locked_one_off_acquisitions_held_for_the_helper()
            .held_or_reserved_count_of_shape(shape)
    }

    /// Read the one-off held under `handle_id` with the registry's lock held.
    #[cfg(test)]
    pub(crate) fn read_the_one_off_held_under<Answer>(
        &self,
        handle_id: &str,
        read: impl FnOnce(Option<&RegisteredHandle>) -> Answer,
    ) -> Answer {
        read(
            self.locked_one_off_acquisitions_held_for_the_helper()
                .held_acquisition(handle_id),
        )
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
        release_surface_share_and_parent_caches_for_handle(sandbox, &handle_id, &removed_handle);
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
pub(crate) fn release_surface_share_and_parent_caches_for_handle(
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
    if let RegisteredHandle::PixelBuffer(_) = removed_handle {
        sandbox
            .host_inner()
            .forget_pixel_buffer_looked_up_through_the_surface_store(handle_id);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if removed_handle.is_storage_buffer_backed() {
        sandbox
            .host_inner()
            .unregister_storage_buffer_from_the_parent_wide_map(handle_id);
    }
}

/// Every release a torn-down helper is owed — each handle it never released
/// and each slot of its processor output pools — with their shared pool
/// entries flagged to be freed as soon as nothing holds them.
pub(crate) fn release_everything_a_torn_down_helper_held(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
) {
    for (handle_id, removed_handle) in registry.drain_handles() {
        removed_handle.flag_its_shared_pool_entry_freed_as_soon_as_unheld(sandbox);
        release_surface_share_and_parent_caches_for_handle(sandbox, &handle_id, &removed_handle);
    }
    let processor_output_pool_slots_drained_at_teardown =
        registry.processor_output_pools().drain_slots();
    for released_slot in processor_output_pool_slots_drained_at_teardown {
        released_slot
            .registered_handle
            .flag_its_shared_pool_entry_freed_as_soon_as_unheld(sandbox);
        release_processor_output_pool_slot(sandbox, released_slot);
    }
}

/// The release a processor output slot is owed when its pool lets it go:
/// everything a released handle of its kind is owed, and its place in the
/// generation index.
pub(crate) fn release_processor_output_pool_slot(
    sandbox: &GpuContextLimitedAccess,
    ReleasedProcessorOutputPoolSlot {
        pool_slot_key,
        registered_handle,
    }: ReleasedProcessorOutputPoolSlot,
) {
    release_surface_share_and_parent_caches_for_handle(sandbox, &pool_slot_key, &registered_handle);
    sandbox.forget_lease_aware_pool_slot(&pool_slot_key);
}

/// Best-effort surface-share release paired with registry eviction, for the
/// handles registered under their own id: every acquire on Linux, where each
/// is checked in; a texture or a tensor storage buffer on macOS, where a pixel
/// buffer's id names its pool slot's registration, which outlives the handle.
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if (cfg!(target_os = "linux")
        || removed_handle.is_texture_backed()
        || removed_handle.is_storage_buffer_backed())
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
