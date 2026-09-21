// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Whether a device stream is still serving its device: the report an owner
//! reads and the recorder only the backend arm that owns the device holds.
//!
//! Named for the device stream rather than for one device class, because a
//! device that dies looks the same whatever it carries.

use std::sync::{Arc, OnceLock};

/// Why a stream stopped serving its device without being asked to.
///
/// Text rather than a core [`crate::core::Error`]: it is read repeatedly off an
/// [`DeviceStreamLivenessReport`] long after the thread that produced it is
/// gone, and `Error` is not `Clone`. What an owner does with it is decide —
/// log it, fail, retry — and every one of those needs the reason to survive
/// being read.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceStreamFailureReason(String);

impl DeviceStreamFailureReason {
    /// State why a stream stopped serving its device.
    pub fn of(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl std::fmt::Display for DeviceStreamFailureReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

// Written by hand rather than derived, and the same text as `Display`: this is
// read in a log field, where the derived `DeviceStreamFailureReason("…")`
// wrapper would be noise around the only part anyone acts on.
impl std::fmt::Debug for DeviceStreamFailureReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Whether a stream is still serving its device, and why it stopped if it is
/// not.
///
/// Handed out and cloned rather than answered off the stream itself, because
/// the thread that would act on the answer is never the one holding the
/// stream: a source owns its stream on the processor and does its work on a
/// publishing thread, so it hands that thread a report instead of the stream.
///
/// A device that dies is otherwise indistinguishable from one that went quiet
/// — a finished reader thread looks exactly like a running one, and stopping
/// the stream still succeeds — so without this an owner has no way to notice,
/// retry, or fail.
///
/// Read-only by construction: stating a failure needs an
/// [`DeviceStreamFailureRecorder`], which only the arm that owns the device
/// holds. An owner cannot forge a death it then reads back as real.
///
/// A failure latches for the life of the stream, and nothing clears it.
/// Restarting delivery on a stream whose device died does not revive it — the
/// device is gone, and a report that forgot would let a source go back to
/// looking healthy, which is the defect this exists to remove. Recovering
/// means opening a new stream, which mints a new report with it.
#[derive(Clone, Debug)]
pub struct DeviceStreamLivenessReport {
    failure_that_ended_the_stream: Arc<OnceLock<DeviceStreamFailureReason>>,
}

impl DeviceStreamLivenessReport {
    /// A report for a stream nothing can stop serving its device.
    ///
    /// The silent null audio backend's whole answer: a stream paced by a timer
    /// against no device has no device to lose, so there is no recorder to
    /// pair this with. Its own constructor rather than a recorder whose failure branch
    /// is unreachable, because "never dies" is part of that arm's design and
    /// deserves to be stated.
    pub fn of_a_stream_that_cannot_fail() -> Self {
        Self {
            failure_that_ended_the_stream: Arc::new(OnceLock::new()),
        }
    }

    /// Why the stream stopped serving its device on its own, or `None` while
    /// it is still serving it.
    ///
    /// A stream its owner stopped deliberately answers `None`: being told to
    /// stop is not a failure, and reporting it as one would make the signal
    /// useless at exactly the moment an owner reads it.
    pub fn failure_that_ended_the_stream(&self) -> Option<DeviceStreamFailureReason> {
        self.failure_that_ended_the_stream.get().cloned()
    }
}

/// The write side of a stream's liveness, held by the arm that owns the device.
///
/// Separate from the report so the direction is structural rather than a rule
/// in prose: an owner reads, an arm states, and neither can do the other's job.
#[derive(Clone, Debug)]
pub struct DeviceStreamFailureRecorder {
    failure_that_ended_the_stream: Arc<OnceLock<DeviceStreamFailureReason>>,
}

impl DeviceStreamFailureRecorder {
    /// Mint the pair a stream opens with: the recorder its own device thread
    /// keeps, and the report its owner reads.
    pub fn recording_into_a_new_report() -> (Self, DeviceStreamLivenessReport) {
        let failure_that_ended_the_stream = Arc::new(OnceLock::new());
        (
            Self {
                failure_that_ended_the_stream: Arc::clone(&failure_that_ended_the_stream),
            },
            DeviceStreamLivenessReport {
                failure_that_ended_the_stream,
            },
        )
    }

    /// Record why the stream stopped serving its device.
    ///
    /// The first reason recorded is the one kept — `OnceLock` is what makes
    /// that a property of the type rather than a convention: a stream on its
    /// way down reports more than once, and the first names the cause where
    /// everything after it names a consequence.
    pub fn record_the_failure_that_ended_the_stream(&self, reason: DeviceStreamFailureReason) {
        let _ = self.failure_that_ended_the_stream.set(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_nothing_has_gone_wrong_with_reports_no_failure() {
        let (_recorder, report) = DeviceStreamFailureRecorder::recording_into_a_new_report();
        assert!(
            report.failure_that_ended_the_stream().is_none(),
            "a report that answered a failure before one happened would fire on every \
             healthy stream, which is worth no more than the silence it replaces"
        );
    }

    /// The arm that cannot lose a device answers the same question, so an
    /// owner writes one piece of code and it means the same thing everywhere.
    #[test]
    fn a_stream_that_cannot_fail_reports_no_failure() {
        assert!(
            DeviceStreamLivenessReport::of_a_stream_that_cannot_fail()
                .failure_that_ended_the_stream()
                .is_none()
        );
    }

    /// The whole point of the shape: the thread that records the failure is
    /// never the thread that reads it.
    #[test]
    fn a_failure_the_arm_records_is_read_through_the_owners_own_clone() {
        let (recorder, report) = DeviceStreamFailureRecorder::recording_into_a_new_report();
        let report_the_publishing_thread_holds = report.clone();

        recorder.record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
            "the device delivered nothing for 25 consecutive waits",
        ));

        assert_eq!(
            report_the_publishing_thread_holds
                .failure_that_ended_the_stream()
                .map(|reason| reason.to_string()),
            Some("the device delivered nothing for 25 consecutive waits".to_string())
        );
    }

    /// A dying stream reports more than once — the read that failed, then the
    /// teardown behind it — and the first is the one that says what happened.
    #[test]
    fn the_first_reason_recorded_is_the_one_the_owner_reads() {
        let (recorder, report) = DeviceStreamFailureRecorder::recording_into_a_new_report();

        recorder.record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
            "what actually killed the stream",
        ));
        recorder.record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
            "a consequence of the first",
        ));

        assert_eq!(
            report
                .failure_that_ended_the_stream()
                .map(|reason| reason.to_string()),
            Some("what actually killed the stream".to_string()),
            "the reason kept has to be the cause, not the last thing the teardown said"
        );
    }

    /// The reason is read in a log field, so its `Debug` is the text and not a
    /// wrapper around it.
    #[test]
    fn a_reason_reads_the_same_whether_it_is_printed_for_a_human_or_a_log_field() {
        let reason = DeviceStreamFailureReason::of("the device went away");
        assert_eq!(format!("{reason:?}"), "the device went away");
        assert_eq!(reason.to_string(), "the device went away");
    }
}
