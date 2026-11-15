// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The public multi-step command recorder handle over `RhiCommandRecorderInner`.

use super::*;

/// Multi-step command-buffer recorder.
///
/// The opaque handle points at a `Box<RhiCommandRecorderInner>`; `Drop`
/// does `Box::from_raw` + drop.
///
/// **Single-owner; deliberately NOT `Clone`.** Recording carries
/// mutable state (`begin()` → `record_*(&mut self)` → `submit_*(&mut
/// self)`) that doesn't survive duplication. Calling `.clone()` on the
/// public handle is a compile error, locked by the `compile_fail`
/// doctest below:
///
/// ```compile_fail
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<streamlib_engine::vulkan::rhi::RhiCommandRecorder>();
/// ```
pub struct RhiCommandRecorder {
    /// Opaque handle to the host's `Box<RhiCommandRecorderInner>`.
    pub(crate) handle: *const c_void,
}

// SAFETY: handle points at a `Box<RhiCommandRecorderInner>`; Inner
// is Send+Sync (Mutex-guarded state, &mut self method dispatch
// further restricts mutation to one thread at a time).
unsafe impl Send for RhiCommandRecorder {}
unsafe impl Sync for RhiCommandRecorder {}

impl RhiCommandRecorder {
    /// Build a recorder against the device's default queue.
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>, label: &str) -> Result<Self> {
        let inner = RhiCommandRecorderInner::new(vulkan_device, label)?;
        Ok(Self::from_inner(inner))
    }

    /// Internal helper: leak a `Box<RhiCommandRecorderInner>` as the
    /// opaque handle.
    pub(crate) fn from_inner(inner: RhiCommandRecorderInner) -> Self {
        let handle = Box::into_raw(Box::new(inner)) as *const c_void;
        Self { handle }
    }

    /// Engine-internal mutable borrow of the host-owned
    /// `RhiCommandRecorderInner`.
    pub(crate) fn host_inner_mut(&mut self) -> &mut RhiCommandRecorderInner {
        // SAFETY: `self.handle` is `Box::into_raw(Box<RhiCommandRecorderInner>)`
        // and `&mut self` guarantees no other reference exists.
        unsafe { &mut *(self.handle as *mut RhiCommandRecorderInner) }
    }

    /// Abandon an in-progress recording after a failed record. See
    /// [`RhiCommandRecorderInner::abort_recording`].
    pub fn abort_recording(&mut self) {
        self.host_inner_mut().abort_recording();
    }

    /// Begin a new recording. See [`RhiCommandRecorderInner::begin`].
    pub fn begin(&mut self) -> Result<()> {
        self.host_inner_mut().begin()
    }

    /// Record an image layout transition. See
    /// [`RhiCommandRecorderInner::record_image_barrier`].
    pub fn record_image_barrier(
        &mut self,
        texture: &Texture,
        from_layout: VulkanLayout,
        to_layout: VulkanLayout,
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        self.host_inner_mut().record_image_barrier(
            texture,
            from_layout,
            to_layout,
            from_stage,
            to_stage,
            from_access,
            to_access,
        )
    }

    /// See [`RhiCommandRecorderInner::record_image_release_to_the_external_queue_family`].
    pub fn record_image_release_to_the_external_queue_family(
        &mut self,
        texture: &Texture,
        layout: VulkanLayout,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_image_release_to_the_external_queue_family(texture, layout)
    }

    /// Buffer memory barrier covering the whole buffer.
    pub fn record_buffer_barrier(
        &mut self,
        buffer: &(impl VulkanBufferLike + ?Sized),
        from_stage: VulkanStage,
        to_stage: VulkanStage,
        from_access: VulkanAccess,
        to_access: VulkanAccess,
    ) -> Result<()> {
        self.host_inner_mut().record_buffer_barrier(
            buffer,
            from_stage,
            to_stage,
            from_access,
            to_access,
        )
    }

    /// Copy image → buffer. See [`RhiCommandRecorderInner::record_copy_image_to_buffer`].
    pub fn record_copy_image_to_buffer(
        &mut self,
        src: &Texture,
        src_layout: VulkanLayout,
        dst: &(impl VulkanBufferLike + ?Sized),
        region: ImageCopyRegion,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_copy_image_to_buffer(src, src_layout, dst, region)
    }

    /// Copy buffer → image.
    pub fn record_copy_buffer_to_image(
        &mut self,
        src: &(impl VulkanBufferLike + ?Sized),
        dst: &Texture,
        dst_layout: VulkanLayout,
        region: ImageCopyRegion,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_copy_buffer_to_image(src, dst, dst_layout, region)
    }

    /// Copy buffer → buffer.
    pub fn record_copy_buffer_to_buffer(
        &mut self,
        src: &(impl VulkanBufferLike + ?Sized),
        dst: &(impl VulkanBufferLike + ?Sized),
        byte_size: u64,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_copy_buffer_to_buffer(src, dst, byte_size)
    }

    /// Copy one whole image into another, same format and extent, with the
    /// layout transitions around it, answering the layout `destination`
    /// comes to rest in.
    ///
    /// Each image is barriered from the layout the caller knows it to be in;
    /// a source known to be UNDEFINED holds nothing and is refused.
    pub fn record_copy_image_to_image(
        &mut self,
        source: &Texture,
        source_known_layout: VulkanLayout,
        destination: &Texture,
        destination_known_layout: VulkanLayout,
    ) -> Result<VulkanLayout> {
        self.host_inner_mut()
            .refuse_an_image_to_image_copy_it_cannot_record(
                source,
                source_known_layout,
                destination,
            )?;
        self.record_image_write_as_transfer_destination(
            destination,
            destination_known_layout,
            |recorder| {
                recorder.record_image_read_as_transfer_source(
                    source,
                    source_known_layout,
                    |recorder| {
                        recorder
                            .host_inner_mut()
                            .record_copy_image_to_image_in_transfer_layouts(source, destination)
                    },
                )?;
                Ok(())
            },
        )
    }

    /// Move `texture` from `known_layout` into `TRANSFER_SRC_OPTIMAL`, record
    /// `record_copy` reading it, and bring it back, answering the layout it
    /// comes to rest in (see [`layout_an_image_rests_in_after_a_transfer`]).
    pub fn record_image_read_as_transfer_source(
        &mut self,
        texture: &Texture,
        known_layout: VulkanLayout,
        record_copy: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<VulkanLayout> {
        let resting_layout = layout_an_image_rests_in_after_a_transfer(known_layout);
        self.record_image_barrier(
            texture,
            known_layout,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::TRANSFER_READ,
        )?;
        record_copy(self)?;
        self.record_image_barrier(
            texture,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            resting_layout,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_READ,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
        )?;
        Ok(resting_layout)
    }

    /// Move `texture` from `known_layout` into `TRANSFER_DST_OPTIMAL`, record
    /// `record_copy` writing it, and bring it back, answering the layout it
    /// comes to rest in (see [`layout_an_image_rests_in_after_a_transfer`]).
    pub fn record_image_write_as_transfer_destination(
        &mut self,
        texture: &Texture,
        known_layout: VulkanLayout,
        record_copy: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<VulkanLayout> {
        let resting_layout = layout_an_image_rests_in_after_a_transfer(known_layout);
        self.record_image_barrier(
            texture,
            known_layout,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
            VulkanAccess::TRANSFER_WRITE,
        )?;
        record_copy(self)?;
        self.record_image_barrier(
            texture,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            resting_layout,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
        )?;
        Ok(resting_layout)
    }

    /// Make every earlier submission's writes to `buffer` visible to a
    /// transfer read of it — submission order alone does not.
    pub fn record_buffer_barrier_before_a_transfer_read(
        &mut self,
        buffer: &(impl VulkanBufferLike + ?Sized),
    ) -> Result<()> {
        self.record_buffer_barrier(
            buffer,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::TRANSFER_READ,
        )
    }

    /// Make a transfer write into `buffer` visible to whoever reads next,
    /// on the GPU or, through a coherent mapping after the host wait, the CPU.
    pub fn record_buffer_barrier_publishing_a_transfer_write(
        &mut self,
        buffer: &(impl VulkanBufferLike + ?Sized),
    ) -> Result<()> {
        self.record_buffer_barrier(
            buffer,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
        )
    }

    /// Compute dispatch.
    pub fn record_dispatch(
        &mut self,
        kernel: &VulkanComputeKernel,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        self.host_inner_mut()
            .record_dispatch(kernel, group_x, group_y, group_z)
    }

    /// Submit signaling a timeline semaphore.
    pub fn submit_signaling_timeline(
        &mut self,
        timeline: &HostVulkanTimelineSemaphore,
        signal_value: u64,
    ) -> Result<()> {
        self.host_inner_mut()
            .submit_signaling_timeline(timeline, signal_value)
    }

    /// Submit without semaphore signaling.
    pub fn submit(&mut self) -> Result<()> {
        self.host_inner_mut().submit()
    }

    /// Submit and block until the GPU completes.
    pub fn submit_and_wait(&mut self) -> Result<()> {
        self.host_inner_mut().submit_and_wait()
    }

    /// Block until the most recent submission drains. No-op when nothing is in
    /// flight. Pairs with [`Self::submit`] for a caller that must do something
    /// between handing the queue its work and waiting on it.
    pub fn wait_for_completion(&mut self) -> Result<()> {
        self.host_inner_mut().wait_for_completion()
    }

    /// Block until the most recent submission drains, on the queue completion
    /// timeline. See
    /// [`RhiCommandRecorderInner::wait_for_completion_on_the_queue_completion_timeline`].
    pub fn wait_for_completion_on_the_queue_completion_timeline(&mut self) -> Result<()> {
        self.host_inner_mut()
            .wait_for_completion_on_the_queue_completion_timeline()
    }

    /// The queue submission the latest accepted submit made.
    pub(crate) fn latest_submission(&mut self) -> HostVulkanQueueSubmissionsSnapshot {
        self.host_inner_mut().latest_submission
    }
}

impl Drop for RhiCommandRecorder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with `Box::into_raw` in `from_inner`.
            unsafe {
                drop(Box::from_raw(self.handle as *mut RhiCommandRecorderInner));
            }
        }
    }
}

impl std::fmt::Debug for RhiCommandRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiCommandRecorder").finish()
    }
}
