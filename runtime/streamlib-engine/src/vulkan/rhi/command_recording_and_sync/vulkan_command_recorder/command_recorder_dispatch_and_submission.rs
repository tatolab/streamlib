// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Compute dispatch, submission with timeline semaphores, and waiting for completion.

use super::*;

impl RhiCommandRecorderInner {
    /// Record a compute dispatch via [`VulkanComputeKernel::record`]
    /// into the recorder's command buffer.
    ///
    /// Bindings + push constants must have been staged on `kernel` via
    /// its `set_*` methods before this call. The kernel's descriptor
    /// set is shared across calls; per the kernel's contract no
    /// concurrent dispatch/record against the same kernel may be in
    /// flight.
    #[tracing::instrument(level = "trace", skip(self, kernel), fields(label = %self.label, group_x, group_y, group_z))]
    pub(crate) fn record_dispatch(
        &mut self,
        kernel: &VulkanComputeKernel,
        group_x: u32,
        group_y: u32,
        group_z: u32,
    ) -> Result<()> {
        self.expect_recording("record_dispatch")?;
        kernel.record(self.command_buffer, group_x, group_y, group_z)
    }

    /// Submit with the given semaphore waits and signals. Mirrors
    /// [`Self::submit_inner`]'s recorder-state and fence bookkeeping so the
    /// next [`Self::begin`] waits correctly.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn submit_with_semaphores(
        &mut self,
        waits: &[vk::SemaphoreSubmitInfo],
        signals: &[vk::SemaphoreSubmitInfo],
    ) -> Result<()> {
        {
            let mut state = self.state.lock();
            if *state != RecorderState::Recording {
                return Err(Error::GpuError(format!(
                    "RhiCommandRecorder '{}': submit_with_semaphores called without an active recording",
                    self.label
                )));
            }
            *state = RecorderState::Idle;
        }

        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': end_command_buffer: {e}",
                        self.label
                    ))
                })?;
        }

        let cmd_info = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(self.command_buffer)
            .build();
        let cmd_infos = [cmd_info];

        let submit = vk::SubmitInfo2::builder()
            .wait_semaphore_infos(waits)
            .command_buffer_infos(&cmd_infos)
            .signal_semaphore_infos(signals)
            .build();

        self.latest_submission = unsafe {
            self.vulkan_device
                .submit_to_queue_naming_its_completion(self.queue, &[submit], self.completion_fence)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': submit_to_queue: {e}",
                        self.label
                    ))
                })?
        };

        self.submission_in_flight = true;

        Ok(())
    }

    /// End recording and submit, signaling `timeline` at `signal_value`
    /// on completion. The recorder's internal completion fence is also
    /// signaled so the next `begin()` waits the right amount.
    ///
    /// `signal_value` MUST be strictly greater than the timeline's
    /// current counter — Vulkan disallows monotonic regressions.
    #[tracing::instrument(level = "trace", skip(self, timeline), fields(label = %self.label, signal_value))]
    pub(crate) fn submit_signaling_timeline(
        &mut self,
        timeline: &HostVulkanTimelineSemaphore,
        signal_value: u64,
    ) -> Result<()> {
        self.submit_inner(Some((timeline.semaphore(), signal_value)))
    }

    /// End recording and submit in one queue submission, optionally
    /// GPU-waiting `wait` (`(timeline, wait_value)`) before the recorded
    /// work runs and optionally signaling `signal`
    /// (`(timeline, signal_value)`) on completion. `None` for either side
    /// omits that half.
    ///
    /// This backs the OPAQUE_FD/CUDA per-frame producer copy
    /// (`copy_texture_to_storage_buffer_and_signal`): the producer waits
    /// on the consumer's `consume_done` timeline before overwriting the
    /// shared buffer, then signals `produce_done` after the copy so the
    /// consumer's `acquire_read` unblocks. Each signal/wait value MUST be
    /// strictly greater than its timeline's current counter (Vulkan
    /// disallows monotonic regressions).
    ///
    /// When both sides are `None` the caller gets a bare submission with
    /// no host wait — use [`Self::submit_and_wait`] if a host-side drain
    /// is needed instead.
    #[cfg(target_os = "linux")]
    #[tracing::instrument(level = "trace", skip(self, wait, signal), fields(label = %self.label))]
    pub(crate) fn submit_waiting_and_signaling_timeline(
        &mut self,
        wait: Option<(&HostVulkanTimelineSemaphore, u64)>,
        signal: Option<(&HostVulkanTimelineSemaphore, u64)>,
    ) -> Result<()> {
        let waits: Vec<vk::SemaphoreSubmitInfo> = wait
            .into_iter()
            .map(|(timeline, value)| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(timeline.semaphore())
                    .value(value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build()
            })
            .collect();
        let signals: Vec<vk::SemaphoreSubmitInfo> = signal
            .into_iter()
            .map(|(timeline, value)| {
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(timeline.semaphore())
                    .value(value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build()
            })
            .collect();
        self.submit_with_semaphores(&waits, &signals)
    }

    /// End recording and submit without semaphore signaling. The
    /// recorder's internal completion fence is signaled so the next
    /// `begin()` blocks on completion.
    #[tracing::instrument(level = "trace", skip(self), fields(label = %self.label))]
    pub(crate) fn submit(&mut self) -> Result<()> {
        self.submit_inner(None)
    }

    /// End recording, submit, and block until the GPU completes.
    /// Convenience for one-shot setup paths and tests; per-frame paths
    /// should prefer [`Self::submit_signaling_timeline`] and have the
    /// caller wait on the timeline at frame boundaries.
    #[tracing::instrument(level = "trace", skip(self), fields(label = %self.label))]
    pub(crate) fn submit_and_wait(&mut self) -> Result<()> {
        self.submit_inner(None)?;
        self.drain_completion_fence("submit_and_wait")
    }

    /// Host-block until this recorder's most recent submission drains,
    /// via its internal completion fence. Use after a
    /// [`Self::submit_waiting_and_signaling_timeline`] whose signal side
    /// is `None`: with no GPU-side signal to order a downstream consumer,
    /// a host-side drain is the only completion guarantee the caller has.
    /// No-op when nothing is in flight (the fence is already signaled).
    #[tracing::instrument(level = "trace", skip(self), fields(label = %self.label))]
    pub(crate) fn wait_for_completion(&mut self) -> Result<()> {
        self.drain_completion_fence("wait_for_completion")
    }

    /// Host-block until this recorder's most recent submission drains, by
    /// waiting on the queue completion timeline value it signals rather than
    /// on the recorder's fence, so the wait needs nothing but this snapshot.
    /// No-op when nothing is in flight.
    #[tracing::instrument(level = "trace", skip(self), fields(label = %self.label))]
    pub(crate) fn wait_for_completion_on_the_queue_completion_timeline(&self) -> Result<()> {
        if !self.submission_in_flight {
            return Ok(());
        }
        self.vulkan_device
            .wait_for_queue_submissions_blocking_counted(
                &self.latest_submission,
                format_args!(
                    "RhiCommandRecorder '{}' on the queue completion timeline",
                    self.label
                ),
            )
    }

    /// Wait out the in-flight submission, then reset the fence and clear the
    /// in-flight flag so the next `begin()` has nothing left to wait for.
    ///
    /// Reclaiming here rather than leaving it to `begin()` is what makes a
    /// caller that waits explicitly pay one stall per submission instead of
    /// two: without it the fence stays marked in flight, and the next
    /// `begin()` blocks a second time on a fence that is already signaled.
    fn drain_completion_fence(&mut self, waited_in: &'static str) -> Result<()> {
        if !self.submission_in_flight {
            return Ok(());
        }
        self.vulkan_device.wait_for_fences_blocking_counted(
            &[self.completion_fence],
            format_args!("RhiCommandRecorder '{}' in {waited_in}", self.label),
        )?;
        unsafe {
            self.device
                .reset_fences(&[self.completion_fence])
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': reset_fences in {waited_in}: {e}",
                        self.label
                    ))
                })?;
        }
        self.submission_in_flight = false;
        Ok(())
    }

    fn submit_inner(&mut self, timeline_signal: Option<(vk::Semaphore, u64)>) -> Result<()> {
        {
            let mut state = self.state.lock();
            if *state != RecorderState::Recording {
                return Err(Error::GpuError(format!(
                    "RhiCommandRecorder '{}': submit called without an active recording",
                    self.label
                )));
            }
            *state = RecorderState::Idle;
        }

        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': end_command_buffer: {e}",
                        self.label
                    ))
                })?;
        }

        let cmd_info = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(self.command_buffer)
            .build();
        let cmd_infos = [cmd_info];

        let signal_infos;
        let submit = match timeline_signal {
            Some((semaphore, value)) => {
                let info = vk::SemaphoreSubmitInfo::builder()
                    .semaphore(semaphore)
                    .value(value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build();
                signal_infos = [info];
                vk::SubmitInfo2::builder()
                    .command_buffer_infos(&cmd_infos)
                    .signal_semaphore_infos(&signal_infos)
                    .build()
            }
            None => vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build(),
        };

        self.latest_submission = unsafe {
            self.vulkan_device
                .submit_to_queue_naming_its_completion(self.queue, &[submit], self.completion_fence)
                .map_err(|e| {
                    Error::GpuError(format!(
                        "RhiCommandRecorder '{}': submit_to_queue: {e}",
                        self.label
                    ))
                })?
        };

        // Only mark in-flight AFTER the queue accepted the submission.
        // If submit failed (or any earlier step did), the flag stays
        // `false` and the next `begin()` skips the fence wait — which
        // would otherwise deadlock on an unsignaled fence.
        self.submission_in_flight = true;

        Ok(())
    }
}
