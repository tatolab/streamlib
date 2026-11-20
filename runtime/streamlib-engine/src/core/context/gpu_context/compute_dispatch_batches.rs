// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// One dispatch inside a batched recording, with its bindings already resolved
/// to registrations.
///
/// Resolution happens before the recording opens, on purpose: a name the shader
/// does not declare, or a surface this graph cannot resolve, is a caller
/// mistake, and refusing it while a command buffer is open would mean unwinding
/// one.
/// Owned rather than borrowing: a kernel and a registration are both opaque
/// `Arc` handles whose clone is a refcount bump, and a cloned registration
/// shares the very layout cell `update_layout` writes — so owning them costs a
/// few increments and spares the caller a second set of vectors to borrow from.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct BatchedComputeKernelDispatch {
    /// The kernel to bind and dispatch. No kernel may appear twice in one
    /// batch — see [`GpuContext::dispatch_compute_kernel_batch`].
    pub kernel: Arc<crate::vulkan::rhi::VulkanComputeKernel>,
    /// Every binding the kernel declares. Bindings do not persist on a kernel,
    /// so this is complete for each dispatch.
    pub bindings: Vec<BatchedComputeKernelDispatchBinding>,
    /// Push-constant payload for this dispatch alone — recorded into the
    /// command buffer, so consecutive dispatches of different kernels keep
    /// their own.
    pub push_constants: Vec<u8>,
    /// `vkCmdDispatch` groupCountX.
    pub group_count_x: u32,
    /// `vkCmdDispatch` groupCountY.
    pub group_count_y: u32,
    /// `vkCmdDispatch` groupCountZ.
    pub group_count_z: u32,
}

/// One resolved binding of a [`BatchedComputeKernelDispatch`].
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct BatchedComputeKernelDispatchBinding {
    /// Descriptor binding number the kernel declares this resource at.
    pub binding: u32,
    /// The surface bound there, as the kind the shader declares it.
    pub surface_bound_resource: SurfaceBoundKernelBindingResource,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl BatchedComputeKernelDispatchBinding {
    /// Stage this binding on `kernel`, as the kind the shader declares it.
    ///
    /// The one place a binding kind becomes a descriptor write: every
    /// dispatch, batched or alone, records its writes through here.
    pub fn write_into_kernel(
        &self,
        kernel: &crate::vulkan::rhi::VulkanComputeKernel,
    ) -> Result<()> {
        match &self.surface_bound_resource {
            SurfaceBoundKernelBindingResource::StorageImage(registration) => {
                kernel.set_storage_image(self.binding, registration.texture())
            }
            SurfaceBoundKernelBindingResource::SampledTexture(registration) => {
                kernel.set_sampled_texture(self.binding, registration.texture())
            }
            SurfaceBoundKernelBindingResource::StorageBuffer(buffer) => {
                kernel.set_storage_buffer_storage(self.binding, buffer)
            }
        }
    }
}

/// The resource a surface id resolved to for one kernel binding, carried as
/// the kind the shader declares it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone)]
pub enum SurfaceBoundKernelBindingResource {
    /// A texture written through `imageStore`, and the layout it is tracked in.
    StorageImage(TextureRegistration),
    /// A texture read through a combined sampler, and the layout it is tracked in.
    SampledTexture(TextureRegistration),
    /// A tensor storage buffer, read and written as an SSBO.
    StorageBuffer(crate::core::rhi::StorageBuffer),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl SurfaceBoundKernelBindingResource {
    /// The layout a storage image's descriptor requires.
    pub const STORAGE_IMAGE_REQUIRED_LAYOUT: VulkanLayout = VulkanLayout::GENERAL;
    /// The layout a combined image sampler's descriptor requires.
    pub const SAMPLED_TEXTURE_REQUIRED_LAYOUT: VulkanLayout =
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL;

    /// The binding kind this resource is bound as.
    pub fn kind(&self) -> crate::core::rhi::SurfaceBoundKernelBindingKind {
        use crate::core::rhi::SurfaceBoundKernelBindingKind;
        match self {
            Self::StorageImage(_) => SurfaceBoundKernelBindingKind::StorageImage,
            Self::SampledTexture(_) => SurfaceBoundKernelBindingKind::SampledTexture,
            Self::StorageBuffer(_) => SurfaceBoundKernelBindingKind::StorageBuffer,
        }
    }

    /// The bound texture and the image layout its descriptor requires, or
    /// `None` for a storage buffer, which has no layout.
    pub fn texture_and_required_layout(&self) -> Option<(&TextureRegistration, VulkanLayout)> {
        match self {
            Self::StorageImage(registration) => {
                Some((registration, Self::STORAGE_IMAGE_REQUIRED_LAYOUT))
            }
            Self::SampledTexture(registration) => {
                Some((registration, Self::SAMPLED_TEXTURE_REQUIRED_LAYOUT))
            }
            Self::StorageBuffer(_) => None,
        }
    }

    /// The image a texture binding names; `None` for a storage buffer, or a
    /// texture that carries no image.
    pub(crate) fn bound_image(&self) -> Option<vulkanalia::vk::Image> {
        self.texture_and_required_layout()
            .and_then(|(registration, _)| registration.texture().vulkan_inner().image())
    }

    /// The bound storage buffer, or `None` for a texture.
    pub fn storage_buffer(&self) -> Option<&crate::core::rhi::StorageBuffer> {
        match self {
            Self::StorageBuffer(buffer) => Some(buffer),
            Self::StorageImage(_) | Self::SampledTexture(_) => None,
        }
    }
}

/// Where a dispatch's binding sits in `recording`, for a refusal to name.
///
/// A recording of one dispatch is the single-dispatch escalate op riding the
/// batch machinery; its caller wrote no batch, so the location names the
/// binding alone rather than a "dispatch 0" that exists only host-side.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn binding_location_in_this_recording(
    recording: &[BatchedComputeKernelDispatch],
    dispatch_index: usize,
    binding: u32,
) -> String {
    if recording.len() == 1 {
        format!("binding {binding}")
    } else {
        format!("dispatch {dispatch_index} of this batch, binding {binding}")
    }
}

impl GpuContext {
    /// Record every dispatch in `batch` into one command buffer, submit once,
    /// and return when that submission has retired.
    ///
    /// The cost this exists to avoid is per-dispatch: N dispatches recorded
    /// separately cost N submissions and N waits — the single-dispatch
    /// escalate op runs as a recording of one through this very method.
    /// Batched, N passes cost one submission and one wait, and a caller that
    /// leaves this call has its writes visible, exactly as after a single
    /// dispatch.
    ///
    /// The wait is on the queue completion timeline value this submission
    /// signals, with nothing shared held: two streams' dispatches record and
    /// submit one after the other, and each then waits only for its own work.
    ///
    /// Each dispatch's bindings are barriered into the layout its descriptor
    /// requires. Those barriers are also the write-then-read edge between
    /// consecutive passes, so pass N+1 observes pass N's stores. An image's
    /// first barrier in the recording takes a wide source scope — whatever
    /// wrote it before the batch is not the batch's to know — and narrows to
    /// compute-to-compute once this recording is the writer.
    ///
    /// One kernel may appear only once. A kernel records into one descriptor
    /// set, so a second bind would retarget the dispatch already recorded
    /// against it — silently, since nothing has executed yet. Refused by name rather than
    /// dispatched wrong.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn dispatch_compute_kernel_batch(
        &self,
        batch: &[BatchedComputeKernelDispatch],
    ) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        for (index, dispatch) in batch.iter().enumerate() {
            if let Some(earlier) = batch[..index]
                .iter()
                .position(|prior| prior.kernel.handle == dispatch.kernel.handle)
            {
                return Err(Error::GpuError(format!(
                    "dispatch {index} of this batch names the kernel dispatch {earlier} already \
                     names; a kernel records into one descriptor set, so binding it twice in one \
                     recording would give dispatch {earlier} this dispatch's bindings. Build a \
                     second kernel, or run them as separate batches"
                )));
            }
        }

        let idle_recorder = self.idle_compute_dispatch_recorders.lock().pop();
        let mut recorder = match idle_recorder {
            Some(recorder) => recorder,
            None => self.create_command_recorder("batched_compute_dispatch")?,
        };
        let completed = self
            .record_and_submit_compute_kernel_batch(&mut recorder, batch)
            .and_then(|()| recorder.wait_for_completion_on_the_queue_completion_timeline());
        self.idle_compute_dispatch_recorders.lock().push(recorder);
        completed?;

        tracing::debug!(
            rhi_op = "dispatch_compute_kernel_batch",
            dispatches = batch.len(),
            "GpuContext::dispatch_compute_kernel_batch — one submission, one wait"
        );
        Ok(())
    }

    /// Lock `latest_submission_per_compute_kernel` once no kernel in
    /// `kernel_keys` has a submission still running, waiting for those
    /// submissions with the lock released.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn lock_latest_submissions_once_these_kernels_are_free(
        &self,
        kernel_keys: &[usize],
    ) -> Result<
        parking_lot::MutexGuard<
            '_,
            HashMap<usize, crate::vulkan::rhi::HostVulkanQueueSubmissionsSnapshot>,
        >,
    > {
        loop {
            let latest_submission_per_compute_kernel =
                self.latest_submission_per_compute_kernel.lock();
            let previous_submissions_of_these_kernels = kernel_keys
                .iter()
                .filter_map(|kernel_key| latest_submission_per_compute_kernel.get(kernel_key))
                .fold(
                    crate::vulkan::rhi::HostVulkanQueueSubmissionsSnapshot::default(),
                    |covered, previous_submission| covered.covering_both(previous_submission),
                );
            if self
                .device
                .inner
                .every_queue_submission_in_snapshot_has_completed(
                    &previous_submissions_of_these_kernels,
                )?
            {
                return Ok(latest_submission_per_compute_kernel);
            }
            drop(latest_submission_per_compute_kernel);
            self.device
                .inner
                .wait_for_queue_submissions_blocking_counted(
                    &previous_submissions_of_these_kernels,
                    format_args!("a compute dispatch waiting for its kernel's previous dispatch"),
                )?;
        }
    }

    /// Begin `recorder`, then record `batch` into it and submit it while
    /// holding `latest_submission_per_compute_kernel`, once every kernel it
    /// names is free of its previous submission.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn record_and_submit_compute_kernel_batch(
        &self,
        recorder: &mut crate::vulkan::rhi::RhiCommandRecorder,
        batch: &[BatchedComputeKernelDispatch],
    ) -> Result<()> {
        let kernel_keys: Vec<usize> = batch
            .iter()
            .map(|dispatch| dispatch.kernel.handle as usize)
            .collect();
        // Begun outside the shared lock, since retiring the recorder's
        // previous submission may wait on its fence. From here on every early
        // return closes the recording, or the next batch's begin() refuses.
        recorder.begin()?;
        let mut latest_submission_per_compute_kernel =
            match self.lock_latest_submissions_once_these_kernels_are_free(&kernel_keys) {
                Ok(locked) => locked,
                Err(e) => {
                    recorder.abort_recording();
                    return Err(e);
                }
            };
        let layout_each_image_landed_in = match Self::record_compute_kernel_batch(recorder, batch) {
            Ok(landed_in) => landed_in,
            Err(e) => {
                recorder.abort_recording();
                return Err(e);
            }
        };
        recorder.submit()?;
        let this_submission = recorder.latest_submission();
        for kernel_key in kernel_keys {
            latest_submission_per_compute_kernel.insert(kernel_key, this_submission);
        }

        // Published between the submit and the wait, not after both: once the
        // queue has taken the recording the transitions belong to the GPU, so a
        // wait that fails must not leave the tracked layouts describing a state
        // the hardware has already left. A batch that failed while recording
        // never reaches here, and its textures are still as they arrived.
        //
        // Per binding, not per image: a cross-process resolve synthesizes a
        // fresh registration per call, so two slots naming one image hold two
        // layout cells — and every cell must learn the layout the recording
        // left the image in, or the stale one is what gets published.
        for binding in batch.iter().flat_map(|dispatch| &dispatch.bindings) {
            let Some((registration, _)) =
                binding.surface_bound_resource.texture_and_required_layout()
            else {
                continue;
            };
            let Some(image) = registration.texture().vulkan_inner().image() else {
                continue;
            };
            if let Some(layout_landed_in) = layout_each_image_landed_in.get(&image) {
                registration.update_layout(*layout_landed_in);
            }
        }
        Ok(())
    }

    /// The recording half of [`Self::dispatch_compute_kernel_batch`], split out
    /// so every failure path funnels through one `abort_recording`.
    ///
    /// Returns the layout each bound texture ends the recording in, for the
    /// caller to publish once the submission has retired.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn record_compute_kernel_batch(
        recorder: &mut crate::vulkan::rhi::RhiCommandRecorder,
        batch: &[BatchedComputeKernelDispatch],
    ) -> Result<HashMap<vulkanalia::vk::Image, VulkanLayout>> {
        use crate::vulkan::rhi::{VulkanAccess, VulkanBufferLike as _, VulkanStage};

        // The layout each image is in *as the recording proceeds*. Tracking it
        // here rather than re-reading the registration is what makes a chain
        // work: pass 1 leaves its output in GENERAL, and barriering pass 2's
        // read from the pre-batch layout — typically UNDEFINED for a freshly
        // acquired texture — would discard exactly the writes pass 2 is there
        // to read.
        //
        // Keyed on the image, which is what a layout belongs to, and not on
        // the registration: a cross-process import synthesizes a fresh
        // registration per resolve, so two passes over one imported surface
        // would otherwise track two independent layouts for one image.
        let mut layout_during_recording: HashMap<vulkanalia::vk::Image, VulkanLayout> =
            HashMap::new();
        // A buffer has no layout to track, only whether this recording has
        // touched it yet — which decides the barrier's source scope as it
        // does for an image.
        let mut buffers_touched_in_this_recording: std::collections::HashSet<
            vulkanalia::vk::Buffer,
        > = std::collections::HashSet::new();

        // The source scope narrows only once the batch is the writer.
        // On a resource's first touch the previous writer is whatever
        // the graph did — a transfer upload, a camera, another node,
        // CUDA — so the wide scope every other entry-from-an-unknown-
        // producer barrier in the engine uses is the only correct one.
        // From the second touch on, this recording wrote it, and
        // compute-to-compute is exactly the dependency to name.
        let barrier_source_stage_and_access_for_touch = |first_touch_in_this_recording: bool| {
            if first_touch_in_this_recording {
                (VulkanStage::ALL_COMMANDS, VulkanAccess::MEMORY_WRITE)
            } else {
                (VulkanStage::COMPUTE_SHADER, VulkanAccess::SHADER_WRITE)
            }
        };

        for (dispatch_index, dispatch) in batch.iter().enumerate() {
            for binding in &dispatch.bindings {
                if let Some(buffer) = binding.surface_bound_resource.storage_buffer() {
                    let (from_stage, from_access) = barrier_source_stage_and_access_for_touch(
                        buffers_touched_in_this_recording.insert(buffer.vk_buffer()),
                    );
                    // Recorded on every touch: it carries the previous pass's
                    // stores to this pass's loads.
                    recorder.record_buffer_barrier(
                        buffer,
                        from_stage,
                        VulkanStage::COMPUTE_SHADER,
                        from_access,
                        VulkanAccess::SHADER_READ | VulkanAccess::SHADER_WRITE,
                    )?;
                    continue;
                }
                let Some((registration, required_layout)) =
                    binding.surface_bound_resource.texture_and_required_layout()
                else {
                    continue;
                };
                let image = registration
                    .texture()
                    .vulkan_inner()
                    .image()
                    .ok_or_else(|| {
                        Error::GpuError(format!(
                            "{} names a texture with no image, which a descriptor cannot be \
                             written from",
                            binding_location_in_this_recording(
                                batch,
                                dispatch_index,
                                binding.binding
                            )
                        ))
                    })?;
                let first_touch_in_this_recording = !layout_during_recording.contains_key(&image);
                let layout_so_far = *layout_during_recording
                    .entry(image)
                    .or_insert_with(|| registration.current_layout());
                let (from_stage, from_access) =
                    barrier_source_stage_and_access_for_touch(first_touch_in_this_recording);
                // Recorded even when the layout already matches: the barrier is
                // carrying the previous pass's stores to this pass's reads, and
                // a same-layout transition is exactly that memory dependency.
                recorder.record_image_barrier(
                    registration.texture(),
                    layout_so_far,
                    required_layout,
                    from_stage,
                    VulkanStage::COMPUTE_SHADER,
                    from_access,
                    VulkanAccess::SHADER_READ | VulkanAccess::SHADER_WRITE,
                )?;
                layout_during_recording.insert(image, required_layout);
            }

            for binding in &dispatch.bindings {
                binding.write_into_kernel(&dispatch.kernel)?;
            }
            // A kernel that declares push constants must be given them even
            // when the payload is empty, so `set_push_constants` produces the
            // size mismatch rather than the dispatch running against whatever
            // the kernel's staged buffer last held.
            if dispatch.kernel.push_constant_size() > 0 || !dispatch.push_constants.is_empty() {
                dispatch
                    .kernel
                    .set_push_constants(&dispatch.push_constants)?;
            }

            recorder.record_dispatch(
                &dispatch.kernel,
                dispatch.group_count_x,
                dispatch.group_count_y,
                dispatch.group_count_z,
            )?;
        }
        Ok(layout_during_recording)
    }
}
