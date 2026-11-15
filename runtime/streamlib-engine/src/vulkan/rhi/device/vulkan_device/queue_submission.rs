// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Mutex;
use std::sync::atomic::Ordering;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Error, Result};
use crate::vulkan::rhi::command_recording_and_sync::vulkan_queue_submission_completion::{
    HostVulkanQueueSubmissionCompletionTimelines, HostVulkanQueueSubmissionsSnapshot,
    HostVulkanSubmissionQueue,
};

use super::HostVulkanDevice;

impl HostVulkanDevice {
    // ---- Thread-safe queue submission ----
    //
    // Vulkan requires external synchronization for vkQueueSubmit on the same
    // VkQueue from multiple threads. NVIDIA's driver also has internal
    // thread-safety issues during concurrent device-level operations.
    // These methods acquire per-queue mutexes before submitting.

    /// Which of this device's queues a handle is; `None` for a handle this
    /// device never handed out. Aliased handles resolve to the first match.
    fn submission_queue_for(&self, queue: vk::Queue) -> Option<HostVulkanSubmissionQueue> {
        if queue == self.queue {
            Some(HostVulkanSubmissionQueue::Graphics)
        } else if queue == self.transfer_queue {
            Some(HostVulkanSubmissionQueue::Transfer)
        } else if self.compute_queue == Some(queue) {
            Some(HostVulkanSubmissionQueue::Compute)
        } else {
            None
        }
    }

    /// Look up the mutex that guards a given queue handle.
    fn mutex_for_queue(&self, queue: vk::Queue) -> &Mutex<()> {
        match self.submission_queue_for(queue) {
            Some(HostVulkanSubmissionQueue::Graphics) | None => &self.graphics_queue_mutex,
            Some(HostVulkanSubmissionQueue::Transfer) => &self.transfer_queue_mutex,
            Some(HostVulkanSubmissionQueue::Compute) => &self.compute_queue_mutex,
        }
    }

    /// Submit command buffers to a queue with per-queue mutex
    /// synchronization, signalling that queue's completion timeline after
    /// the last submit. A queue this device did not hand out is refused:
    /// no completion timeline would record its work, so the shared pools
    /// could free memory it still names.
    pub unsafe fn submit_to_queue(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()> {
        unsafe { self.submit_to_queue_naming_its_completion(queue, submits, fence) }.map(|_| ())
    }

    /// [`Self::submit_to_queue`], answering the snapshot that names this
    /// submission alone, for a caller that waits on its own work rather than
    /// on a fence.
    pub(crate) unsafe fn submit_to_queue_naming_its_completion(
        &self,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<HostVulkanQueueSubmissionsSnapshot> {
        let Some(submission_queue) = self.submission_queue_for(queue) else {
            return Err(Error::GpuError(format!(
                "queue submission to {queue:?}, a queue this device did not hand out"
            )));
        };
        let Some(completion_timelines) = self.queue_submission_completion_timelines.as_ref() else {
            return Err(Error::GpuError(
                "queue submission to a device whose queue completion timelines are already \
                 destroyed"
                    .into(),
            ));
        };
        let _lock = self
            .mutex_for_queue(queue)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let this_submission = unsafe {
            completion_timelines.submit_signaling_completion_timeline(
                submission_queue,
                queue,
                submits,
                fence,
            )
        }?;
        self.queue_submission_count.fetch_add(1, Ordering::Relaxed);
        Ok(this_submission)
    }

    /// The latest submission made so far on every queue of this device.
    pub(crate) fn snapshot_of_queue_submissions_made_so_far(
        &self,
    ) -> HostVulkanQueueSubmissionsSnapshot {
        self.queue_submission_completion_timelines
            .as_ref()
            .map(HostVulkanQueueSubmissionCompletionTimelines::snapshot_of_submissions_made_so_far)
            .unwrap_or_default()
    }

    /// Whether every queue submission the snapshot names has completed,
    /// read without blocking.
    pub(crate) fn every_queue_submission_in_snapshot_has_completed(
        &self,
        snapshot: &HostVulkanQueueSubmissionsSnapshot,
    ) -> Result<bool> {
        match self.queue_submission_completion_timelines.as_ref() {
            Some(completion_timelines) => {
                completion_timelines.every_submission_in_snapshot_has_completed(snapshot)
            }
            None => Ok(true),
        }
    }

    /// Block until every queue submission the snapshot names has completed
    /// or `timeout` passes; `Ok(false)` on timeout.
    pub(crate) fn wait_for_every_queue_submission_in_snapshot(
        &self,
        snapshot: &HostVulkanQueueSubmissionsSnapshot,
        timeout: std::time::Duration,
    ) -> Result<bool> {
        match self.queue_submission_completion_timelines.as_ref() {
            Some(completion_timelines) => {
                completion_timelines.wait_for_every_submission_in_snapshot(snapshot, timeout)
            }
            None => Ok(true),
        }
    }

    /// Block until `fences` signal, counting the stall.
    ///
    /// The counted seam for work that trades submissions for stalls: batching N
    /// dispatches into one recording is a claim about how many times the caller
    /// blocks, and this is where that is observable rather than inferred from
    /// elapsed time. Routed through by every
    /// [`RhiCommandRecorder`](crate::vulkan::rhi::RhiCommandRecorder) drain and by
    /// [`VulkanComputeKernel::dispatch`](crate::vulkan::rhi::VulkanComputeKernel::dispatch);
    /// other engine fence waits call `vkWaitForFences` directly and are not
    /// counted, so a reading is only about a batch when nothing else on this
    /// device is recording.
    ///
    /// `waited_by` takes `format_args!` so the caller's label is rendered only
    /// when the wait actually fails — this sits on the per-frame path.
    pub fn wait_for_fences_blocking_counted(
        &self,
        fences: &[vk::Fence],
        waited_by: std::fmt::Arguments<'_>,
    ) -> Result<()> {
        self.counted_host_waits_on_gpu_work_count
            .fetch_add(1, Ordering::Relaxed);
        unsafe { self.device.wait_for_fences(fences, true, u64::MAX) }
            .map(|_| ())
            .map_err(|e| Error::GpuError(format!("{waited_by}: wait_for_fences: {e}")))
    }

    /// Block until every queue submission `snapshot` names has completed,
    /// counting the stall as [`Self::wait_for_fences_blocking_counted`] does.
    pub(crate) fn wait_for_queue_submissions_blocking_counted(
        &self,
        snapshot: &HostVulkanQueueSubmissionsSnapshot,
        waited_by: std::fmt::Arguments<'_>,
    ) -> Result<()> {
        self.counted_host_waits_on_gpu_work_count
            .fetch_add(1, Ordering::Relaxed);
        self.wait_for_every_queue_submission_in_snapshot(snapshot, std::time::Duration::MAX)
            .map(|_| ())
            .map_err(|e| Error::GpuError(format!("{waited_by}: {e}")))
    }

    /// Every `vkQueueSubmit2` this device has made.
    pub fn queue_submission_count(&self) -> usize {
        self.queue_submission_count.load(Ordering::Relaxed)
    }

    /// Waits made through [`Self::wait_for_fences_blocking_counted`] and
    /// [`Self::wait_for_queue_submissions_blocking_counted`].
    pub fn counted_host_waits_on_gpu_work_count(&self) -> usize {
        self.counted_host_waits_on_gpu_work_count
            .load(Ordering::Relaxed)
    }

    /// `vkDeviceWaitIdle` with the threading discipline the Vulkan spec
    /// requires. Per the spec, `vkDeviceWaitIdle` is externally
    /// synchronized over **every `VkQueue` the device owns** — calling
    /// it concurrently with `vkQueueSubmit2` / `vkQueuePresentKHR` on
    /// any of those queues trips
    /// `UNASSIGNED-Threading-MultipleThreads-Write`. Acquires all
    /// per-queue mutexes (in a fixed lock order so there's no deadlock
    /// against [`Self::submit_to_queue`] / [`Self::present_to_queue`],
    /// each of which takes only a single queue mutex) before calling through.
    ///
    /// Every in-tree consumer should reach `vkDeviceWaitIdle` through
    /// this helper, not raw `self.device().device_wait_idle()`.
    pub fn wait_idle(&self) -> Result<()> {
        // Fixed lock order: graphics → transfer → compute. `submit_to_queue` / `present_to_queue`
        // only ever take ONE queue mutex, so any order here is deadlock-
        // safe against them; this order matches struct-field declaration
        // for readability.
        let _g = self
            .graphics_queue_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _t = self
            .transfer_queue_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _c = self
            .compute_queue_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { self.device.device_wait_idle() }
            .map_err(|e| Error::GpuError(format!("device_wait_idle failed: {e}")))
    }
}
