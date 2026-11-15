// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `CudaSurfaceAdapter<D>` — CUDA-typed `SurfaceAdapter`.
//!
//! Generic over the device flavor: `D = HostVulkanDevice` for host-side
//! adapter use today (allocate OPAQUE_FD-exportable resources, register,
//! hand fds to CUDA). The four trait methods on `VulkanRhiDevice`
//! (`device()`, `queue()`, `queue_family_index()`, `submit_to_queue()`)
//! are everything the adapter needs from the device; the timeline-
//! semaphore type is picked up via `D::Privilege::TimelineSemaphore`
//! and abstracted behind `VulkanTimelineSemaphoreLike` so wait + signal
//! works against either flavor.
//!
//! The adapter:
//! - Owns a registry of registered surfaces keyed by [`SurfaceId`].
//! - Waits on the timeline semaphore at the start of every acquire so
//!   prior CUDA / GPU work has drained before the host touches the
//!   buffer.
//! - Signals the next timeline value on guard drop so the next acquire
//!   wakes up.
//!
//! Per-acquire host *work* (e.g. `vkCmdCopyImageToBuffer`) is **not
//! present** here, on purpose: CUDA imports the OPAQUE_FD memory once at
//! registration time and dispatches kernels in its own context. The
//! timeline semaphore is the only sync surface that has to cross the
//! Vulkan↔CUDA boundary per acquire.
//!
//! For the *host-pipeline producer* shape (in-process processor pushing
//! frames into a registered surface so a subprocess customer can
//! `acquire_read` them GPU-resident), see
//! [`CudaSurfaceAdapter::submit_host_copy_image_to_buffer`]. That path
//! is GPU-signaled and stays out of the per-acquire codepath.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib_consumer_rhi::{DevicePrivilege, TextureFormat, VulkanRhiDevice};
use streamlib_surface_adapter::Registry;
use vulkanalia::vk;

use crate::state::SurfaceState;

mod adapter_persistent_submit_context;
mod host_copy_image_to_buffer_submit;
mod host_surface_registration;
mod peer_timeline_wait;
mod surface_acquire;
mod surface_adapter_for_cuda;

use adapter_persistent_submit_context::AdapterPersistentSubmitContext;

/// The `consume_done` timeline a releasing reader signals, and the value it signals.
type ConsumeDoneSignal<D> = (
    Arc<<<D as VulkanRhiDevice>::Privilege as DevicePrivilege>::TimelineSemaphore>,
    u64,
);

/// Default per-acquire timeline-wait timeout. Long enough to cover any
/// realistic GPU queue depth; short enough that a deadlock turns into
/// an `AdapterError::SyncTimeout` rather than wedging the consumer.
const DEFAULT_TIMELINE_WAIT: Duration = Duration::from_secs(5);

/// CUDA-targeted [`SurfaceAdapter`](streamlib_surface_adapter::SurfaceAdapter)
/// implementation. Generic over the device flavor — instantiate as
/// `CudaSurfaceAdapter<HostVulkanDevice>` host-side; future cdylib work
/// (#589/#590) will add a consumer-flavor instantiation.
pub struct CudaSurfaceAdapter<D: VulkanRhiDevice> {
    device: Arc<D>,
    surfaces: Registry<SurfaceState<D::Privilege>>,
    /// Per-acquire timeline-wait timeout. Adjustable via
    /// [`Self::with_acquire_timeout`].
    acquire_timeout: Duration,
    /// Persistent command pool + buffer + completion fence for the
    /// host-pipeline producer copy path
    /// ([`Self::submit_host_copy_image_to_buffer`]). Lazy-init on
    /// first submit; reused with `vkResetCommandPool` after each
    /// submission's fence has signaled. See
    /// [`AdapterPersistentSubmitContext`] for the full contract.
    /// Single-threaded caller convention; multi-threaded callers
    /// serialize through this mutex.
    #[cfg(target_os = "linux")]
    submit_ctx: Mutex<Option<AdapterPersistentSubmitContext>>,
    /// Counts how many times the persistent submit context was
    /// (re)created — used by tests to lock the amortisation invariant
    /// from #620 (steady-state submits must not grow live pools).
    submit_ctx_create_count: AtomicUsize,
}

impl<D: VulkanRhiDevice> CudaSurfaceAdapter<D> {
    /// Construct an empty adapter bound to `device`.
    pub fn new(device: Arc<D>) -> Self {
        Self {
            device,
            surfaces: Registry::new(),
            acquire_timeout: DEFAULT_TIMELINE_WAIT,
            #[cfg(target_os = "linux")]
            submit_ctx: Mutex::new(None),
            submit_ctx_create_count: AtomicUsize::new(0),
        }
    }

    /// Number of times the adapter has materialised its persistent
    /// command pool. Stays at 0 before the first
    /// [`Self::submit_host_copy_image_to_buffer`] call, becomes 1 on
    /// first invocation, and stays at 1 across every subsequent
    /// submit.
    ///
    /// Hidden from the public docs because callers shouldn't depend
    /// on it; tests use it to lock #620's amortisation invariant.
    #[doc(hidden)]
    pub fn submit_pool_create_count(&self) -> usize {
        self.submit_ctx_create_count.load(Ordering::Relaxed)
    }

    /// Override the per-acquire timeline-wait timeout. Default 5 s.
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Returns the underlying device for callers (test harnesses, the
    /// `CudaContext`, raw-handle escape hatches) that need it.
    pub fn device(&self) -> &Arc<D> {
        &self.device
    }
}

#[cfg(target_os = "linux")]
impl<D: VulkanRhiDevice> Drop for CudaSurfaceAdapter<D> {
    fn drop(&mut self) {
        let mut guard = match self.submit_ctx.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(ctx) = guard.take() {
            ctx.destroy(self.device.device());
        }
    }
}

/// Snapshot taken under the registry lock so the timeline wait can run
/// unlocked. `read_holders` / `write_held` are already incremented;
/// rollback paths decrement them on failure. `peer_timeline` is the
/// side this caller waits on (`produce_done` for read paths,
/// `consume_done` for write paths) per single-writer-per-edge.
struct ReadAcquired<P: DevicePrivilege> {
    peer_timeline: Arc<P::TimelineSemaphore>,
    buffer: vk::Buffer,
    size: vk::DeviceSize,
}

struct WriteAcquired<P: DevicePrivilege> {
    peer_timeline: Arc<P::TimelineSemaphore>,
    buffer: vk::Buffer,
    size: vk::DeviceSize,
}

/// Image-flavored read snapshot — sibling of [`ReadAcquired`] for the
/// `acquire_texture` path. Carries the `vk::Image` handle + the
/// dimensions / format the cdylib needs to build a
/// `cudaTextureObject_t`.
struct ImageReadAcquired<P: DevicePrivilege> {
    peer_timeline: Arc<P::TimelineSemaphore>,
    image: vk::Image,
    width: u32,
    height: u32,
    format: TextureFormat,
}

/// Image-flavored write snapshot — sibling of [`WriteAcquired`] for
/// the `acquire_surface` path.
struct ImageWriteAcquired<P: DevicePrivilege> {
    peer_timeline: Arc<P::TimelineSemaphore>,
    image: vk::Image,
    width: u32,
    height: u32,
    format: TextureFormat,
}
