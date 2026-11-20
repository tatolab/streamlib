// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Whether a codec node may try again to get what its GPU refused it, an
//! encoder's session or a decoder's device: always at first, never again
//! after a refusal that a later try would meet again, and, while the refusal
//! is for want of something another client of the GPU may free, once a retry
//! interval after each refusal.

use std::time::{Duration, Instant};

/// Why a codec node's GPU refused it what it asked for.
#[derive(Debug)]
pub enum VideoCodecRefusal {
    /// The GPU lacked something another of its clients may free (a video
    /// session, memory, the BAR1 aperture), so a later try may succeed.
    ForNow(String),
    /// A later try would meet the same refusal.
    ForGood(String),
}

impl VideoCodecRefusal {
    /// The refusal's message, whichever kind it is.
    pub fn into_message(self) -> String {
        match self {
            Self::ForNow(message) | Self::ForGood(message) => message,
        }
    }
}

/// How long after a refusal for now the next try is made.
pub(crate) const VIDEO_CODEC_RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
enum VideoCodecRetryState {
    #[default]
    MayTry,
    RefusedForNow {
        next_try_not_before: Instant,
        tries_refused: u64,
    },
    FailedForGood,
}

/// The retry state of one codec node.
#[derive(Debug, Default)]
pub(crate) struct VideoCodecRetryGate {
    state: VideoCodecRetryState,
}

impl VideoCodecRetryGate {
    /// Whether a try may be made at `now`.
    pub(crate) fn a_try_may_be_made_at(&self, now: Instant) -> bool {
        match self.state {
            VideoCodecRetryState::MayTry => true,
            VideoCodecRetryState::RefusedForNow {
                next_try_not_before,
                ..
            } => now >= next_try_not_before,
            VideoCodecRetryState::FailedForGood => false,
        }
    }

    /// Record a try refused for now at `now`, answering how many tries in a
    /// row were refused.
    pub(crate) fn refused_for_now_at(&mut self, now: Instant) -> u64 {
        let tries_refused = match self.state {
            VideoCodecRetryState::RefusedForNow { tries_refused, .. } => tries_refused + 1,
            VideoCodecRetryState::MayTry | VideoCodecRetryState::FailedForGood => 1,
        };
        self.state = VideoCodecRetryState::RefusedForNow {
            next_try_not_before: now + VIDEO_CODEC_RETRY_INTERVAL,
            tries_refused,
        };
        tries_refused
    }

    /// Record a try that failed for a reason a later try would meet again.
    pub(crate) fn failed_for_good(&mut self) {
        self.state = VideoCodecRetryState::FailedForGood;
    }

    /// Record a try that succeeded, answering how many tries were refused for
    /// now before it.
    pub(crate) fn succeeded(&mut self) -> u64 {
        let tries_refused = match self.state {
            VideoCodecRetryState::RefusedForNow { tries_refused, .. } => tries_refused,
            VideoCodecRetryState::MayTry | VideoCodecRetryState::FailedForGood => 0,
        };
        self.state = VideoCodecRetryState::MayTry;
        tries_refused
    }
}
