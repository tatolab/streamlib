// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Whether the device behind an audio built-in is still serving it.
//!
//! Both audio built-ins do their work on a thread of their own, and both would
//! otherwise wait out the whole run against a device that stopped: a dead
//! stream keeps its shape, so nothing in a loop that only watches its ring can
//! tell a quiet microphone from an absent one. This is where that question is
//! asked, once, for both of them.

use streamlib::sdk::context::AudioStreamLivenessReport;

/// Whether a built-in's worker loop still has a device under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceServingState {
    StillServing,
    StoppedServing,
}

/// Ask a stream's liveness report whether its device is still serving, saying
/// why when it is not.
///
/// Both the line and the answer, because either alone leaves the defect: a log
/// nothing reads does not stop a loop from waiting forever on a dead device,
/// and a loop that stops without saying why sends its reader looking at the
/// consumer.
///
/// `what_a_stopped_device_means_here` is the caller's own sentence because the
/// consequence differs either side of the seam — a source stops publishing, a
/// sink stops playing — and a reader acts on the consequence.
pub fn ask_whether_the_device_is_still_serving(
    liveness_report: &AudioStreamLivenessReport,
    what_a_stopped_device_means_here: &str,
) -> AudioDeviceServingState {
    let Some(reason) = liveness_report.failure_that_ended_the_stream() else {
        return AudioDeviceServingState::StillServing;
    };
    tracing::error!(%reason, "{what_a_stopped_device_means_here}");
    AudioDeviceServingState::StoppedServing
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::context::AudioStreamFailureReason;

    #[test]
    fn a_device_that_is_serving_is_reported_as_serving() {
        assert_eq!(
            ask_whether_the_device_is_still_serving(
                &AudioStreamLivenessReport::of_a_stream_that_has_not_failed(),
                "this must not be said about a healthy device",
            ),
            AudioDeviceServingState::StillServing
        );
    }

    #[test]
    fn a_device_that_stopped_is_reported_as_stopped() {
        let liveness_report = AudioStreamLivenessReport::of_a_stream_that_has_not_failed();
        liveness_report.record_the_failure_that_ended_the_stream(AudioStreamFailureReason::of(
            "the device delivered nothing for 25 consecutive waits",
        ));

        assert_eq!(
            ask_whether_the_device_is_still_serving(&liveness_report, "the device stopped"),
            AudioDeviceServingState::StoppedServing
        );
    }
}
