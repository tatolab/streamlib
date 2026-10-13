// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A completion timeline per engine queue, signalled by every engine queue
//! submission, so whether earlier GPU work has finished can be read without
//! a device-idle wait.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use super::vulkan_sync::HostVulkanTimelineSemaphore;
use crate::core::{Error, Result};

/// One of the engine device's queues, each owning a submission mutex and a
/// completion timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostVulkanSubmissionQueue {
    Graphics,
    Transfer,
    VideoEncode,
    VideoDecode,
    Compute,
}

impl HostVulkanSubmissionQueue {
    const COUNT: usize = 5;
    const ALL: [Self; Self::COUNT] = [
        Self::Graphics,
        Self::Transfer,
        Self::VideoEncode,
        Self::VideoDecode,
        Self::Compute,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

/// The value each engine queue's completion timeline was last handed when
/// the snapshot was taken; zero for a queue nothing had been submitted to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostVulkanQueueSubmissionsSnapshot {
    latest_submitted_value_per_queue: [u64; HostVulkanSubmissionQueue::COUNT],
}

impl HostVulkanQueueSubmissionsSnapshot {
    /// Whether the snapshot names no submission at all.
    pub fn names_no_submission(&self) -> bool {
        self.latest_submitted_value_per_queue
            .iter()
            .all(|&value| value == 0)
    }

    /// The later of the two snapshots on every queue.
    pub fn covering_both(&self, other: &Self) -> Self {
        let mut latest_submitted_value_per_queue = self.latest_submitted_value_per_queue;
        for (value, other_value) in latest_submitted_value_per_queue
            .iter_mut()
            .zip(other.latest_submitted_value_per_queue)
        {
            *value = (*value).max(other_value);
        }
        Self {
            latest_submitted_value_per_queue,
        }
    }
}

struct HostVulkanQueueSubmissionCompletionTimeline {
    timeline_semaphore: HostVulkanTimelineSemaphore,
    /// The value the latest successful submission to this queue signals.
    /// Advanced only under the queue's submission mutex, so it increases in
    /// submission order as the timeline's signal values must.
    latest_submitted_value: AtomicU64,
}

/// The per-queue completion timelines of one engine device.
pub(crate) struct HostVulkanQueueSubmissionCompletionTimelines {
    device: vulkanalia::Device,
    timeline_per_queue:
        [HostVulkanQueueSubmissionCompletionTimeline; HostVulkanSubmissionQueue::COUNT],
}

impl HostVulkanQueueSubmissionCompletionTimelines {
    /// One timeline per queue, each at zero. Work on different queues
    /// completes out of order, so a timeline shared between queues could be
    /// signalled below its current value.
    pub(crate) fn new(device: &vulkanalia::Device) -> Result<Self> {
        let create_timeline = || -> Result<HostVulkanQueueSubmissionCompletionTimeline> {
            Ok(HostVulkanQueueSubmissionCompletionTimeline {
                timeline_semaphore: HostVulkanTimelineSemaphore::new(device, 0)?,
                latest_submitted_value: AtomicU64::new(0),
            })
        };
        Ok(Self {
            device: device.clone(),
            timeline_per_queue: [
                create_timeline()?,
                create_timeline()?,
                create_timeline()?,
                create_timeline()?,
                create_timeline()?,
            ],
        })
    }

    /// `vkQueueSubmit2` with a signal of `submission_queue`'s completion
    /// timeline appended to the last submit's signal list.
    ///
    /// # Safety
    /// The caller holds `submission_queue`'s submission mutex for the whole
    /// call, and `submits` satisfies `vkQueueSubmit2`'s valid usage.
    pub(crate) unsafe fn submit_signaling_completion_timeline(
        &self,
        submission_queue: HostVulkanSubmissionQueue,
        queue: vk::Queue,
        submits: &[vk::SubmitInfo2],
        fence: vk::Fence,
    ) -> Result<()> {
        let Some(last_submit) = submits.last() else {
            return unsafe { self.device.queue_submit2(queue, submits, fence) }
                .map(|_| ())
                .map_err(|e| Error::GpuError(format!("queue_submit2 failed: {e}")));
        };
        let completion_timeline = &self.timeline_per_queue[submission_queue.index()];
        let next_submitted_value = completion_timeline
            .latest_submitted_value
            .load(Ordering::Relaxed)
            + 1;

        let callers_signal_infos: &[vk::SemaphoreSubmitInfo] =
            if last_submit.signal_semaphore_info_count == 0 {
                &[]
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        last_submit.signal_semaphore_infos,
                        last_submit.signal_semaphore_info_count as usize,
                    )
                }
            };
        let mut signal_infos_with_completion_timeline =
            Vec::with_capacity(callers_signal_infos.len() + 1);
        signal_infos_with_completion_timeline.extend_from_slice(callers_signal_infos);
        signal_infos_with_completion_timeline.push(
            vk::SemaphoreSubmitInfo::builder()
                .semaphore(completion_timeline.timeline_semaphore.semaphore())
                .value(next_submitted_value)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .build(),
        );

        let mut submits_with_completion_signal = submits.to_vec();
        if let Some(last_submit_with_completion_signal) = submits_with_completion_signal.last_mut()
        {
            last_submit_with_completion_signal.signal_semaphore_info_count =
                signal_infos_with_completion_timeline.len() as u32;
            last_submit_with_completion_signal.signal_semaphore_infos =
                signal_infos_with_completion_timeline.as_ptr();
        }

        unsafe {
            self.device
                .queue_submit2(queue, &submits_with_completion_signal, fence)
        }
        .map_err(|e| Error::GpuError(format!("queue_submit2 failed: {e}")))?;
        completion_timeline
            .latest_submitted_value
            .store(next_submitted_value, Ordering::Release);
        Ok(())
    }

    /// The latest successful submission on every queue.
    pub(crate) fn snapshot_of_submissions_made_so_far(&self) -> HostVulkanQueueSubmissionsSnapshot {
        let mut latest_submitted_value_per_queue = [0; HostVulkanSubmissionQueue::COUNT];
        for submission_queue in HostVulkanSubmissionQueue::ALL {
            latest_submitted_value_per_queue[submission_queue.index()] = self.timeline_per_queue
                [submission_queue.index()]
            .latest_submitted_value
            .load(Ordering::Acquire);
        }
        HostVulkanQueueSubmissionsSnapshot {
            latest_submitted_value_per_queue,
        }
    }

    /// Whether every submission the snapshot names has completed, without
    /// blocking.
    pub(crate) fn every_submission_in_snapshot_has_completed(
        &self,
        snapshot: &HostVulkanQueueSubmissionsSnapshot,
    ) -> Result<bool> {
        for submission_queue in HostVulkanSubmissionQueue::ALL {
            let submitted_value =
                snapshot.latest_submitted_value_per_queue[submission_queue.index()];
            if submitted_value == 0 {
                continue;
            }
            let completed_value = self.timeline_per_queue[submission_queue.index()]
                .timeline_semaphore
                .current_value()?;
            if completed_value < submitted_value {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Block until every submission the snapshot names has completed or
    /// `timeout` passes; `Ok(false)` on timeout.
    pub(crate) fn wait_for_every_submission_in_snapshot(
        &self,
        snapshot: &HostVulkanQueueSubmissionsSnapshot,
        timeout: Duration,
    ) -> Result<bool> {
        let mut semaphores = Vec::with_capacity(HostVulkanSubmissionQueue::COUNT);
        let mut values = Vec::with_capacity(HostVulkanSubmissionQueue::COUNT);
        for submission_queue in HostVulkanSubmissionQueue::ALL {
            let submitted_value =
                snapshot.latest_submitted_value_per_queue[submission_queue.index()];
            if submitted_value == 0 {
                continue;
            }
            semaphores.push(
                self.timeline_per_queue[submission_queue.index()]
                    .timeline_semaphore
                    .semaphore(),
            );
            values.push(submitted_value);
        }
        if semaphores.is_empty() {
            return Ok(true);
        }
        let wait_info = vk::SemaphoreWaitInfo::builder()
            .flags(vk::SemaphoreWaitFlags::empty())
            .semaphores(&semaphores)
            .values(&values)
            .build();
        let timeout_ns = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
        let outcome =
            unsafe { self.device.wait_semaphores(&wait_info, timeout_ns) }.map_err(|e| {
                Error::GpuError(format!(
                    "vkWaitSemaphores on the queue completion timelines: {e}"
                ))
            })?;
        Ok(outcome != vk::SuccessCode::TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    use crate::vulkan::rhi::{HostVulkanDevice, HostVulkanTimelineSemaphore, RhiCommandRecorder};

    use super::HostVulkanQueueSubmissionsSnapshot;

    fn test_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(device) => Some(device),
            Err(error) => {
                tracing::warn!("skipping — no Vulkan device available: {error}");
                None
            }
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_queue_submission_snapshot_reads_completed_once_its_recorders_fence_was_waited() {
        let Some(device) = test_device() else {
            return;
        };
        let empty_snapshot = HostVulkanQueueSubmissionsSnapshot::default();
        assert!(empty_snapshot.names_no_submission());
        assert!(
            device
                .every_queue_submission_in_snapshot_has_completed(&empty_snapshot)
                .expect("reading an empty snapshot")
        );
        assert!(
            device
                .wait_for_every_queue_submission_in_snapshot(&empty_snapshot, Duration::ZERO)
                .expect("waiting on an empty snapshot")
        );

        let mut recorder =
            RhiCommandRecorder::new(&device, "queue-submission-snapshot-test").expect("recorder");
        recorder.begin().expect("begin");
        recorder.submit().expect("submit");
        let snapshot_after_submit = device.snapshot_of_queue_submissions_made_so_far();
        assert!(
            !snapshot_after_submit.names_no_submission(),
            "a recorder submit must advance its queue's completion timeline"
        );
        recorder.wait_for_completion().expect("recorder fence wait");
        assert!(
            device
                .every_queue_submission_in_snapshot_has_completed(&snapshot_after_submit)
                .expect("reading the snapshot"),
            "the recorder's fence signalled, so the submission the snapshot names completed"
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_queue_submission_snapshot_reads_incomplete_until_its_gated_submission_runs() {
        let Some(device) = test_device() else {
            return;
        };
        let host_signalled_gate =
            HostVulkanTimelineSemaphore::new(device.device(), 0).expect("gate timeline");
        let gate_wait = [vk::SemaphoreSubmitInfo::builder()
            .semaphore(host_signalled_gate.semaphore())
            .value(1)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .build()];
        let callers_own_signal_timeline =
            HostVulkanTimelineSemaphore::new(device.device(), 0).expect("caller's timeline");
        let callers_own_signal = [vk::SemaphoreSubmitInfo::builder()
            .semaphore(callers_own_signal_timeline.semaphore())
            .value(7)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .build()];

        let mut recorder = RhiCommandRecorder::new(&device, "gated-queue-submission-snapshot-test")
            .expect("recorder");
        recorder.begin().expect("begin");
        recorder
            .submit_with_semaphores(&gate_wait, &callers_own_signal)
            .expect("gated submit");
        let snapshot_of_gated_submission = device.snapshot_of_queue_submissions_made_so_far();

        let completed_while_gated =
            device.every_queue_submission_in_snapshot_has_completed(&snapshot_of_gated_submission);
        let bounded_wait_while_gated = device.wait_for_every_queue_submission_in_snapshot(
            &snapshot_of_gated_submission,
            Duration::from_millis(20),
        );

        host_signalled_gate.signal_host(1).expect("open the gate");
        let bounded_wait_after_gate_opened = device.wait_for_every_queue_submission_in_snapshot(
            &snapshot_of_gated_submission,
            Duration::from_secs(5),
        );
        recorder.wait_for_completion().expect("recorder fence wait");

        assert!(
            !completed_while_gated.expect("reading the gated snapshot"),
            "a submission still waiting on its gate must read incomplete"
        );
        assert!(
            !bounded_wait_while_gated.expect("bounded wait while gated"),
            "the bounded wait must time out while the gate is shut"
        );
        assert!(
            bounded_wait_after_gate_opened.expect("bounded wait after the gate opened"),
            "the bounded wait must complete once the gate opens"
        );
        assert!(
            device
                .every_queue_submission_in_snapshot_has_completed(&snapshot_of_gated_submission)
                .expect("reading the snapshot after completion")
        );
        assert_eq!(
            callers_own_signal_timeline
                .current_value()
                .expect("caller's timeline value"),
            7,
            "the caller's own signal must survive the appended completion signal"
        );
        if let Some(validation_counts) = device.validation_layer_message_counts() {
            assert_eq!(
                validation_counts.error_count, 0,
                "the appended completion signal must be valid usage"
            );
        }
    }
}
