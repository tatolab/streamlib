// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanSurfaceAdapter<D>` — Vulkan-typed `SurfaceAdapter`.
//!
//! Generic over the device flavor: `D = HostVulkanDevice` for host-side
//! adapter use (allocate + register), `D = ConsumerVulkanDevice` for
//! cdylib subprocess use (import + register). The four trait methods on
//! `VulkanRhiDevice` (`device()`, `queue()`, `queue_family_index()`,
//! `submit_to_queue()`) are everything the adapter needs from the
//! device; the timeline semaphore type is picked up via
//! `D::Privilege::TimelineSemaphore` and abstracted behind
//! `VulkanTimelineSemaphoreLike` so the same wait + signal calls work
//! against either flavor.
//!
//! The adapter:
//! - Owns a registry of registered surfaces keyed by [`SurfaceId`].
//! - Waits on the timeline semaphore at the start of every acquire so
//!   prior GPU work has drained.
//! - Issues a layout transition into the consumer's expected layout
//!   (`SHADER_READ_ONLY_OPTIMAL` for read, `GENERAL` for write — this
//!   covers compute, transfer, and color-attachment use cases).
//! - Signals the next timeline value on guard drop so the next acquire
//!   wakes up.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use streamlib::adapter_support::{
    DevicePrivilege, VulkanRhiDevice, VulkanTextureLike, VulkanTimelineSemaphoreLike,
};
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, Registry, StreamlibSurface, SurfaceAdapter, SurfaceId,
    SurfaceRegistration, VkImageInfo, WriteGuard,
};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::state::{HostSurfaceRegistration, SurfaceState, VulkanLayout};
use crate::view::{VulkanReadView, VulkanWriteView};

/// Default per-acquire timeline-wait timeout. Long enough to cover any
/// realistic compositor queue; short enough that a deadlock turns into
/// an `AdapterError::SyncTimeout` rather than wedging the consumer.
const DEFAULT_TIMELINE_WAIT: Duration = Duration::from_secs(5);

/// Vulkan-native [`SurfaceAdapter`] implementation. Generic over the
/// device flavor — instantiate as `VulkanSurfaceAdapter<HostVulkanDevice>`
/// host-side or `VulkanSurfaceAdapter<ConsumerVulkanDevice>` cdylib-side.
pub struct VulkanSurfaceAdapter<D: VulkanRhiDevice> {
    device: Arc<D>,
    surfaces: Registry<SurfaceState<D::Privilege>>,
    /// Per-acquire timeline wait timeout. Adjustable via
    /// [`Self::with_acquire_timeout`].
    acquire_timeout: Duration,
}

impl<D: VulkanRhiDevice> VulkanSurfaceAdapter<D> {
    /// Construct an empty adapter bound to `device`.
    pub fn new(device: Arc<D>) -> Self {
        Self {
            device,
            surfaces: Registry::new(),
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
    pub fn device(&self) -> &Arc<D> {
        &self.device
    }

    /// Register an allocated (or imported) surface with this adapter.
    ///
    /// `id` is assigned by the host (typically from the surface-share
    /// service); it MUST be unique across the adapter's lifetime.
    /// Returns an error if `id` is already registered.
    pub fn register_host_surface(
        &self,
        id: SurfaceId,
        registration: HostSurfaceRegistration<D::Privilege>,
    ) -> Result<(), AdapterError> {
        let inserted = self.surfaces.register(
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
        if !inserted {
            return Err(AdapterError::SurfaceNotFound { surface_id: id });
        }
        Ok(())
    }

    /// Drop a registered surface. Pending guards keep the underlying
    /// `Arc<TimelineSemaphore>` alive; the next acquire returns
    /// [`AdapterError::SurfaceNotFound`].
    pub fn unregister_host_surface(&self, id: SurfaceId) -> bool {
        self.surfaces.unregister(id).is_some()
    }

    /// Snapshot the registry size — primarily for tests and
    /// observability.
    pub fn registered_count(&self) -> usize {
        self.surfaces.len()
    }

    fn make_image_info(&self, image: vk::Image) -> VkImageInfo {
        // Best-effort image info — fields the adapter doesn't track
        // (memory binding, ycbcr conversion) stay zeroed. Skia and other
        // VkImageInfoExt consumers can extend this once the adapter
        // tracks more per-surface state.
        let _ = image; // future-proof: consumers will read this once filled
        VkImageInfo {
            format: 0,
            tiling: vk::ImageTiling::OPTIMAL.as_raw(),
            usage_flags: 0,
            sample_count: vk::SampleCountFlags::_1.bits(),
            level_count: 1,
            queue_family: self.device.queue_family_index(),
            memory_handle: 0,
            memory_offset: 0,
            memory_size: 0,
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
    ) -> Result<Option<ReadAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces.try_begin_read(id, |state| {
            let timeline = Arc::clone(&state.timeline);
            let wait_value = state.current_release_value;
            let image = state
                .texture
                .image()
                .ok_or(AdapterError::SurfaceNotFound { surface_id: id })?;
            let from = state.current_layout;
            state.last_acquire_value = wait_value;
            Ok(ReadAcquired {
                timeline,
                wait_value,
                image,
                from,
                info: self.make_image_info(image),
            })
        })
    }

    fn try_begin_write(
        &self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteAcquired<D::Privilege>>, AdapterError> {
        let id = surface.id;
        self.surfaces.try_begin_write(id, |state| {
            let timeline = Arc::clone(&state.timeline);
            let wait_value = state.current_release_value;
            let image = state
                .texture
                .image()
                .ok_or(AdapterError::SurfaceNotFound { surface_id: id })?;
            let from = state.current_layout;
            state.last_acquire_value = wait_value;
            Ok(WriteAcquired {
                timeline,
                wait_value,
                image,
                from,
                info: self.make_image_info(image),
            })
        })
    }

    /// Wait + transition + commit-layout for a successful read acquire.
    fn finalize_read(
        &self,
        surface_id: SurfaceId,
        acquired: ReadAcquired<D::Privilege>,
    ) -> Result<vk::ImageLayout, AdapterError> {
        if acquired
            .timeline
            .wait(acquired.wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            // Roll back: the acquire had bumped read_holders; undo it.
            self.surfaces.rollback_read(surface_id);
            return Err(AdapterError::SyncTimeout {
                duration: self.acquire_timeout,
            });
        }

        let to = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        if let Err(err) = self.transition_layout_sync(acquired.image, acquired.from.vk(), to) {
            self.surfaces.rollback_read(surface_id);
            return Err(err);
        }
        self.surfaces.with_mut(surface_id, |state| {
            state.current_layout = VulkanLayout(to.as_raw());
        });
        Ok(to)
    }

    fn finalize_write(
        &self,
        surface_id: SurfaceId,
        acquired: WriteAcquired<D::Privilege>,
    ) -> Result<vk::ImageLayout, AdapterError> {
        if acquired
            .timeline
            .wait(acquired.wait_value, self.acquire_timeout.as_nanos() as u64)
            .is_err()
        {
            self.surfaces.rollback_write(surface_id);
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
            self.surfaces.rollback_write(surface_id);
            return Err(err);
        }
        self.surfaces.with_mut(surface_id, |state| {
            state.current_layout = VulkanLayout(to.as_raw());
        });
        Ok(to)
    }
}

/// Snapshot taken under the registry lock so the timeline wait + layout
/// transition can run unlocked. `read_holders` / `write_held` are
/// already incremented; rollback paths decrement them on failure.
struct ReadAcquired<P: DevicePrivilege> {
    timeline: Arc<P::TimelineSemaphore>,
    wait_value: u64,
    image: vk::Image,
    from: VulkanLayout,
    info: VkImageInfo,
}

struct WriteAcquired<P: DevicePrivilege> {
    timeline: Arc<P::TimelineSemaphore>,
    wait_value: u64,
    image: vk::Image,
    from: VulkanLayout,
    info: VkImageInfo,
}

impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for VulkanSurfaceAdapter<D> {
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
                return Err(AdapterError::WriteContended {
                    surface_id: surface.id,
                    holder: self.surfaces.describe_contention(surface.id),
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
        // Inner Option: `None` means "not the last reader, skip signal".
        // Outer Option: `None` means "surface raced an unregister".
        let signal: Option<Option<(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>, u64)>> =
            self.surfaces.with_mut(surface_id, |state| {
                debug_assert!(state.read_holders > 0, "read release without acquire");
                state.dec_read_holders();
                if state.read_holders > 0 {
                    return None;
                }
                let next = state.next_release_value();
                state.current_release_value = next;
                Some((Arc::clone(&state.timeline), next))
            });
        let signal = match signal {
            Some(s) => s,
            None => {
                tracing::warn!(
                    ?surface_id,
                    "end_read_access on unknown surface — racing unregister"
                );
                return;
            }
        };
        if let Some((timeline, value)) = signal {
            if let Err(e) = timeline.signal_host(value) {
                tracing::error!(?surface_id, %value, %e, "timeline signal failed on read release");
            }
        }
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        let signal: Option<(Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore>, u64)> =
            self.surfaces.with_mut(surface_id, |state| {
                debug_assert!(state.write_held, "write release without acquire");
                state.set_write_held(false);
                let next = state.next_release_value();
                state.current_release_value = next;
                (Arc::clone(&state.timeline), next)
            });
        let (timeline, value) = match signal {
            Some(s) => s,
            None => {
                tracing::warn!(
                    ?surface_id,
                    "end_write_access on unknown surface — racing unregister"
                );
                return;
            }
        };
        if let Err(e) = timeline.signal_host(value) {
            tracing::error!(?surface_id, %value, %e, "timeline signal failed on write release");
        }
    }
}

