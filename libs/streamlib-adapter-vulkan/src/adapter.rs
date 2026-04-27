// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanSurfaceAdapter` — host-side `SurfaceAdapter` implementation
//! that hands a host-allocated `VkImage` to consumers as a Vulkan-typed
//! [`crate::VulkanReadView`] / [`crate::VulkanWriteView`].
//!
//! The adapter:
//! - Owns a registry of host-registered surfaces keyed by [`SurfaceId`].
//! - Waits on the timeline semaphore at the start of every acquire so
//!   prior GPU work has drained.
//! - Issues a layout transition into the consumer's expected layout
//!   (`SHADER_READ_ONLY_OPTIMAL` for read, `GENERAL` for write — this
//!   covers compute, transfer, and color-attachment use cases).
//! - Signals the next timeline value on guard drop so the next acquire
//!   wakes up.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use streamlib::adapter_support::{VulkanDevice, VulkanTimelineSemaphore};
use streamlib::core::rhi::StreamTexture;
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, SurfaceId, VkImageInfo,
    WriteGuard,
};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::state::{HostSurfaceRegistration, SurfaceState, VulkanLayout};
use crate::view::{VulkanReadView, VulkanWriteView};

/// Default per-acquire timeline-wait timeout. Long enough to cover any
/// realistic compositor queue; short enough that a deadlock turns into
/// an `AdapterError::SyncTimeout` rather than wedging the consumer.
const DEFAULT_TIMELINE_WAIT: Duration = Duration::from_secs(5);

/// Vulkan-native [`SurfaceAdapter`] implementation.
///
/// Construct with [`Self::new`] passing the host's [`VulkanDevice`].
/// Register host-allocated surfaces with [`Self::register_host_surface`];
/// consumers acquire scoped access through the standard
/// [`SurfaceAdapter::acquire_read`] / [`SurfaceAdapter::acquire_write`]
/// API or via the [`crate::VulkanContext`] convenience.
pub struct VulkanSurfaceAdapter {
    device: Arc<VulkanDevice>,
    surfaces: Mutex<HashMap<SurfaceId, SurfaceState>>,
    /// Per-acquire timeline wait timeout. Adjustable via
    /// [`Self::with_acquire_timeout`].
    acquire_timeout: Duration,
}

impl VulkanSurfaceAdapter {
    /// Construct an empty adapter bound to `device`.
    pub fn new(device: Arc<VulkanDevice>) -> Self {
        Self {
            device,
            surfaces: Mutex::new(HashMap::new()),
            acquire_timeout: DEFAULT_TIMELINE_WAIT,
        }
    }

    /// Override the per-acquire timeline-wait timeout. Default 5 s.
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Returns the underlying device for callers (test harnesses, the
    /// `VulkanContext`, raw-handle escape hatches) that need it.
    pub fn device(&self) -> &Arc<VulkanDevice> {
        &self.device
    }

    /// Register a host-allocated surface with this adapter.
    ///
    /// `id` is assigned by the host (typically from the surface-share
    /// service); it MUST be unique across the adapter's lifetime.
    /// Returns an error if `id` is already registered.
    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration,
    ) -> Result<(), AdapterError> {
        let mut map = self.surfaces.lock();
        if map.contains_key(&id) {
            return Err(AdapterError::SurfaceNotFound { surface_id: id });
        }
        map.insert(
            id,
            SurfaceState {
                surface_id: id,
                texture: registration.texture,
                timeline: registration.timeline,
                current_layout: registration.initial_layout,
                read_holders: 0,
                write_held: false,
                last_acquire_value: 0,
                current_release_value: 0,
            },
        );
        Ok(())
    }

    /// Drop a registered surface. Pending guards keep the underlying
    /// `Arc<VulkanTimelineSemaphore>` alive; the next acquire returns
    /// [`AdapterError::SurfaceNotFound`].
    pub fn unregister_host_surface(&self, id: SurfaceId) -> bool {
        self.surfaces.lock().remove(&id).is_some()
    }

    /// Snapshot the registry size — primarily for tests and
    /// observability.
    pub fn registered_count(&self) -> usize {
        self.surfaces.lock().len()
    }

    fn make_image_info(&self, texture: &StreamTexture) -> VkImageInfo {
        // Best-effort image info — fields the adapter doesn't track
        // (memory binding, ycbcr conversion) stay zeroed. Skia and other
        // VkImageInfoExt consumers can extend this once
        // VulkanTexture exposes more accessors.
        VkImageInfo {
            format: 0,
            tiling: vk::ImageTiling::OPTIMAL.as_raw(),
            usage_flags: 0,
            sample_count: vk::SampleCountFlags::_1.bits(),
            level_count: 1,
            queue_family: self.device.queue_family_index(),
            memory_handle: 0,
            memory_offset: 0,
            memory_size: ((texture.width() as u64)
                * (texture.height() as u64)
                * (texture.format().bytes_per_pixel() as u64)),
            memory_property_flags: 0,
            protected: 0,
            ycbcr_conversion: 0,
            _reserved: [0; 16],
        }
    }

    /// Submit a one-shot command buffer that transitions `image` from
    /// `from` to `to`. Layout barriers are issued via Vulkan 1.3+
    /// `cmd_pipeline_barrier2`; we use queue-family-foreign transitions
    /// to support cross-process handoff.
    ///
    /// Synchronous: blocks until the GPU has executed the barrier,
    /// because the next consumer needs the new layout to be visible.
    fn transition_layout_sync(
        &self,
        image: vk::Image,
        from: vk::ImageLayout,
        to: vk::ImageLayout,
    ) -> Result<(), AdapterError> {
        if from == to {
            return Ok(());
        }
        let device = self.device.device();
        let queue = self.device.queue();
        let qf = self.device.queue_family_index();

        // Single-shot command pool + buffer.
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(qf)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .build();
        let pool = unsafe { device.create_command_pool(&pool_info, None) }
            .map_err(|e| AdapterError::IpcDisconnected {
                reason: format!("create_command_pool: {e}"),
            })?;

        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let cmd_buffers = unsafe { device.allocate_command_buffers(&alloc_info) }
            .map_err(|e| {
                unsafe { device.destroy_command_pool(pool, None) };
                AdapterError::IpcDisconnected {
                    reason: format!("allocate_command_buffers: {e}"),
                }
            })?;
        let cmd = cmd_buffers[0];

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        if let Err(e) = unsafe { device.begin_command_buffer(cmd, &begin_info) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("begin_command_buffer: {e}"),
            });
        }

        let image_barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(
                vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE,
            )
            .old_layout(from)
            .new_layout(to)
            .src_queue_family_index(qf)
            .dst_queue_family_index(qf)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::builder()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .base_mip_level(0)
                    .level_count(1)
                    .base_array_layer(0)
                    .layer_count(1)
                    .build(),
            )
            .build();

        let image_barriers = [image_barrier];
        let dep_info = vk::DependencyInfo::builder()
            .image_memory_barriers(&image_barriers)
            .build();
        unsafe { device.cmd_pipeline_barrier2(cmd, &dep_info) };

        if let Err(e) = unsafe { device.end_command_buffer(cmd) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("end_command_buffer: {e}"),
            });
        }

        let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build()];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build();

        if let Err(e) = unsafe {
            self.device.submit_to_queue(queue, &[submit], vk::Fence::null())
        } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("submit_to_queue: {e}"),
            });
        }

        // Block until the layout transition has executed. The next
        // consumer's view assumes the new layout is visible.
        if let Err(e) = unsafe { device.queue_wait_idle(queue) } {
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(AdapterError::IpcDisconnected {
                reason: format!("queue_wait_idle: {e}"),
            });
        }

        unsafe { device.destroy_command_pool(pool, None) };
        Ok(())
    }

    fn try_begin_read(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadAcquired>, AdapterError> {
        let mut map = self.surfaces.lock();
        let state = map
            .get_mut(&surface.id)
            .ok_or(AdapterError::SurfaceNotFound { surface_id: surface.id })?;
        if state.write_held {
            return Ok(None);
        }
        // Snapshot what we need before we drop the lock so the wait /
        // layout transition runs unlocked. Counters are committed below.
        let timeline = Arc::clone(&state.timeline);
        let wait_value = state.current_release_value;
        let image = state
            .texture
            .vulkan_inner()
            .image()
            .ok_or(AdapterError::SurfaceNotFound { surface_id: surface.id })?;
        let from = state.current_layout;
        state.read_holders += 1;
        state.last_acquire_value = wait_value;
        Ok(Some(ReadAcquired {
            timeline,
            wait_value,
            image,
            from,
            info: self.make_image_info(&state.texture),
        }))
    }

    fn try_begin_write(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteAcquired>, AdapterError> {
        let mut map = self.surfaces.lock();
        let state = map
            .get_mut(&surface.id)
            .ok_or(AdapterError::SurfaceNotFound { surface_id: surface.id })?;
        if state.write_held || state.read_holders > 0 {
            return Ok(None);
        }
        let timeline = Arc::clone(&state.timeline);
        let wait_value = state.current_release_value;
        let image = state
            .texture
            .vulkan_inner()
            .image()
            .ok_or(AdapterError::SurfaceNotFound { surface_id: surface.id })?;
        let from = state.current_layout;
        state.write_held = true;
        state.last_acquire_value = wait_value;
        Ok(Some(WriteAcquired {
            timeline,
            wait_value,
            image,
            from,
            info: self.make_image_info(&state.texture),
        }))
    }

    /// Wait + transition + commit-layout for a successful read acquire.
    fn finalize_read(
        &self,
        surface_id: SurfaceId,
        acquired: ReadAcquired,
    ) -> Result<vk::ImageLayout, AdapterError> {
        if acquired
            .timeline
            .wait(acquired.wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            // Roll back: the acquire had bumped read_holders; undo it.
            self.rollback_read(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }

        let to = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        if let Err(err) = self.transition_layout_sync(acquired.image, acquired.from.vk(), to) {
            self.rollback_read(surface_id);
            return Err(err);
        }
        let mut map = self.surfaces.lock();
        if let Some(state) = map.get_mut(&surface_id) {
            state.current_layout = VulkanLayout(to.as_raw());
        }
        Ok(to)
    }

    fn finalize_write(
        &self,
        surface_id: SurfaceId,
        acquired: WriteAcquired,
    ) -> Result<vk::ImageLayout, AdapterError> {
        if acquired
            .timeline
            .wait(acquired.wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.rollback_write(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }

        // GENERAL covers compute, transfer, and color-attachment writes.
        // Picking COLOR_ATTACHMENT_OPTIMAL would force a re-transition
        // every time a customer wants to use the image as a transfer
        // destination; GENERAL is the right shape for the v1 adapter.
        let to = vk::ImageLayout::GENERAL;
        if let Err(err) = self.transition_layout_sync(acquired.image, acquired.from.vk(), to) {
            self.rollback_write(surface_id);
            return Err(err);
        }
        let mut map = self.surfaces.lock();
        if let Some(state) = map.get_mut(&surface_id) {
            state.current_layout = VulkanLayout(to.as_raw());
        }
        Ok(to)
    }

    fn rollback_read(&self, surface_id: SurfaceId) {
        let mut map = self.surfaces.lock();
        if let Some(state) = map.get_mut(&surface_id) {
            state.read_holders = state.read_holders.saturating_sub(1);
        }
    }

    fn rollback_write(&self, surface_id: SurfaceId) {
        let mut map = self.surfaces.lock();
        if let Some(state) = map.get_mut(&surface_id) {
            state.write_held = false;
        }
    }
}

/// Snapshot taken under the registry lock so the timeline wait + layout
/// transition can run unlocked. `read_holders` / `write_held` are
/// already incremented; rollback paths decrement them on failure.
struct ReadAcquired {
    timeline: Arc<VulkanTimelineSemaphore>,
    wait_value: u64,
    image: vk::Image,
    from: VulkanLayout,
    info: VkImageInfo,
}

struct WriteAcquired {
    timeline: Arc<VulkanTimelineSemaphore>,
    wait_value: u64,
    image: vk::Image,
    from: VulkanLayout,
    info: VkImageInfo,
}

impl SurfaceAdapter for VulkanSurfaceAdapter {
    type ReadView<'g> = VulkanReadView<'g>;
    type WriteView<'g> = VulkanWriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let acquired = match self.try_begin_read(surface)? {
            Some(a) => a,
            None => {
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: "writer".to_string(),
                });
            }
        };
        let info = acquired.info;
        let image = acquired.image;
        let layout = self.finalize_read(surface.id, acquired)?;
        Ok(ReadGuard::new(
            self,
            surface.id,
            VulkanReadView {
                image,
                layout,
                info,
                _marker: PhantomData,
            },
        ))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let acquired = match self.try_begin_write(surface)? {
            Some(a) => a,
            None => {
                let map = self.surfaces.lock();
                let holder = match map.get(&surface.id) {
                    Some(s) if s.write_held => "writer".to_string(),
                    Some(s) => format!("{} reader(s)", s.read_holders),
                    None => "unknown".to_string(),
                };
                drop(map);
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder,
                });
            }
        };
        let info = acquired.info;
        let image = acquired.image;
        let layout = self.finalize_write(surface.id, acquired)?;
        Ok(WriteGuard::new(
            self,
            surface.id,
            VulkanWriteView {
                image,
                layout,
                info,
                _marker: PhantomData,
            },
        ))
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        let acquired = match self.try_begin_read(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let info = acquired.info;
        let image = acquired.image;
        let layout = self.finalize_read(surface.id, acquired)?;
        Ok(Some(ReadGuard::new(
            self,
            surface.id,
            VulkanReadView {
                image,
                layout,
                info,
                _marker: PhantomData,
            },
        )))
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        let acquired = match self.try_begin_write(surface)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let info = acquired.info;
        let image = acquired.image;
        let layout = self.finalize_write(surface.id, acquired)?;
        Ok(Some(WriteGuard::new(
            self,
            surface.id,
            VulkanWriteView {
                image,
                layout,
                info,
                _marker: PhantomData,
            },
        )))
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        let (timeline, value) = {
            let mut map = self.surfaces.lock();
            let state = match map.get_mut(&surface_id) {
                Some(s) => s,
                None => {
                    tracing::warn!(
                        ?surface_id,
                        "end_read_access on unknown surface — racing unregister"
                    );
                    return;
                }
            };
            debug_assert!(state.read_holders > 0, "read release without acquire");
            state.read_holders = state.read_holders.saturating_sub(1);
            // Only the last reader advances the timeline — concurrent
            // reads share a single release boundary.
            if state.read_holders > 0 {
                return;
            }
            let next = state.next_release_value();
            state.current_release_value = next;
            (Arc::clone(&state.timeline), next)
        };
        if let Err(e) = timeline.signal_host(value) {
            tracing::error!(?surface_id, %value, %e, "timeline signal failed on read release");
        }
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        let (timeline, value) = {
            let mut map = self.surfaces.lock();
            let state = match map.get_mut(&surface_id) {
                Some(s) => s,
                None => {
                    tracing::warn!(
                        ?surface_id,
                        "end_write_access on unknown surface — racing unregister"
                    );
                    return;
                }
            };
            debug_assert!(state.write_held, "write release without acquire");
            state.write_held = false;
            let next = state.next_release_value();
            state.current_release_value = next;
            (Arc::clone(&state.timeline), next)
        };
        if let Err(e) = timeline.signal_host(value) {
            tracing::error!(?surface_id, %value, %e, "timeline signal failed on write release");
        }
    }
}
