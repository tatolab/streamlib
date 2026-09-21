// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The rule every video capture arm stamps its frames under: a frame carries
//! the instant its device captured it, on the machine's monotonic clock, and a
//! device stamp is taken only when it is usable.
//!
//! A platform flag saying a stamp is monotonic is necessary and not
//! sufficient. `vivid` sets `V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC` honestly and
//! still stamps every frame about nine tenths of a frame period in the future,
//! so a stamp ahead of the engine's own clock at dequeue is clamped to that
//! instant and counted rather than trusted.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// What a device reported about when it captured one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceReportedCaptureStamp {
    /// A stamp the device reports on the machine's monotonic clock.
    OnTheMachineMonotonicClock {
        /// The device's capture instant, in nanoseconds.
        capture_timestamp_ns: i64,
    },
    /// No stamp, or one on a clock the engine cannot join to its own.
    OffTheMachineMonotonicClock,
}

/// Resolves each frame's capture instant for one opened device.
///
/// Shared between an arm's stream and whichever thread its frames are
/// dequeued on, so it is read and written through `&self`.
#[derive(Debug)]
pub struct VideoCaptureInstantResolver {
    device_name: String,
    unusable_stamp_already_reported: AtomicBool,
    future_stamp_already_reported: AtomicBool,
    future_capture_stamps_clamped_to_dequeue: AtomicU64,
}

impl VideoCaptureInstantResolver {
    /// A resolver for the device named `device_name`, which every line it
    /// logs names.
    pub fn for_device(device_name: impl Into<String>) -> Self {
        Self {
            device_name: device_name.into(),
            unusable_stamp_already_reported: AtomicBool::new(false),
            future_stamp_already_reported: AtomicBool::new(false),
            future_capture_stamps_clamped_to_dequeue: AtomicU64::new(0),
        }
    }

    /// The frame's capture instant in nanoseconds, given what the device
    /// reported and the engine's own monotonic clock read when the frame was
    /// dequeued.
    ///
    /// A usable stamp is taken as the device gave it. A missing, zero or
    /// off-clock stamp falls back to `dequeued_at_ns` and is reported once for
    /// the device. A stamp ahead of `dequeued_at_ns` is clamped to it and
    /// counted, and the first one is reported.
    pub fn resolve_capture_timestamp_ns(
        &self,
        device_stamp: DeviceReportedCaptureStamp,
        dequeued_at_ns: i64,
    ) -> i64 {
        let capture_timestamp_ns = match device_stamp {
            DeviceReportedCaptureStamp::OnTheMachineMonotonicClock {
                capture_timestamp_ns,
            } if capture_timestamp_ns > 0 => capture_timestamp_ns,
            DeviceReportedCaptureStamp::OnTheMachineMonotonicClock { .. } => {
                self.report_an_unusable_stamp_once(
                    "stamps its frames zero on the machine's monotonic clock",
                );
                return dequeued_at_ns;
            }
            DeviceReportedCaptureStamp::OffTheMachineMonotonicClock => {
                self.report_an_unusable_stamp_once(
                    "does not stamp its frames on the machine's monotonic clock",
                );
                return dequeued_at_ns;
            }
        };
        if capture_timestamp_ns <= dequeued_at_ns {
            return capture_timestamp_ns;
        }
        self.future_capture_stamps_clamped_to_dequeue
            .fetch_add(1, Ordering::Relaxed);
        if !self
            .future_stamp_already_reported
            .swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                camera = %self.device_name,
                ahead_of_dequeue_ns = capture_timestamp_ns - dequeued_at_ns,
                "camera stamped a frame after the instant it was dequeued; clamping \
                 every such stamp to its dequeue instant and counting it"
            );
        }
        dequeued_at_ns
    }

    /// How many device stamps have been ahead of their dequeue instant and
    /// were clamped to it.
    pub fn future_capture_stamps_clamped_to_dequeue(&self) -> u64 {
        self.future_capture_stamps_clamped_to_dequeue
            .load(Ordering::Relaxed)
    }

    fn report_an_unusable_stamp_once(&self, what_the_device_does: &str) {
        if !self
            .unusable_stamp_already_reported
            .swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                camera = %self.device_name,
                "camera {what_the_device_does}; stamping its frames with the engine's \
                 monotonic clock at dequeue instead"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tracing_subscriber::layer::{Context, SubscriberExt};

    const DEQUEUED_AT_NS: i64 = 5_000_000_000;

    #[derive(Default)]
    struct WarningCount(AtomicU64);

    struct WarningCountingLayer(Arc<WarningCount>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarningCountingLayer {
        fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
            if *event.metadata().level() == tracing::Level::WARN {
                self.0.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn warnings_logged_while(resolve_frames: impl FnOnce()) -> u64 {
        let warnings = Arc::new(WarningCount::default());
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(WarningCountingLayer(Arc::clone(&warnings))),
            resolve_frames,
        );
        warnings.0.load(Ordering::Relaxed)
    }

    fn on_the_monotonic_clock(capture_timestamp_ns: i64) -> DeviceReportedCaptureStamp {
        DeviceReportedCaptureStamp::OnTheMachineMonotonicClock {
            capture_timestamp_ns,
        }
    }

    #[test]
    fn a_monotonic_stamp_before_dequeue_is_the_frames_capture_instant() {
        let resolver = VideoCaptureInstantResolver::for_device("a camera");
        assert_eq!(
            resolver.resolve_capture_timestamp_ns(
                on_the_monotonic_clock(DEQUEUED_AT_NS - 33_000_000),
                DEQUEUED_AT_NS
            ),
            DEQUEUED_AT_NS - 33_000_000
        );
        assert_eq!(resolver.future_capture_stamps_clamped_to_dequeue(), 0);
    }

    #[test]
    fn a_stamp_at_the_dequeue_instant_is_trusted_and_not_counted() {
        let resolver = VideoCaptureInstantResolver::for_device("a camera");
        assert_eq!(
            resolver.resolve_capture_timestamp_ns(
                on_the_monotonic_clock(DEQUEUED_AT_NS),
                DEQUEUED_AT_NS
            ),
            DEQUEUED_AT_NS
        );
        assert_eq!(resolver.future_capture_stamps_clamped_to_dequeue(), 0);
    }

    /// `vivid`'s measured behaviour: an honest monotonic flag on a stamp about
    /// nine tenths of a frame period ahead of the instant it was dequeued.
    #[test]
    fn a_monotonic_stamp_from_the_future_is_clamped_to_dequeue_and_counted() {
        let resolver = VideoCaptureInstantResolver::for_device("vivid");
        let nine_tenths_of_a_30_fps_period_ns = 29_800_000;
        for frame in 0..3 {
            assert_eq!(
                resolver.resolve_capture_timestamp_ns(
                    on_the_monotonic_clock(DEQUEUED_AT_NS + nine_tenths_of_a_30_fps_period_ns),
                    DEQUEUED_AT_NS
                ),
                DEQUEUED_AT_NS,
                "frame {frame}"
            );
        }
        assert_eq!(resolver.future_capture_stamps_clamped_to_dequeue(), 3);
    }

    #[test]
    fn a_zero_stamp_falls_back_to_the_dequeue_instant_and_is_not_counted_as_clamped() {
        let resolver = VideoCaptureInstantResolver::for_device("a camera");
        assert_eq!(
            resolver.resolve_capture_timestamp_ns(on_the_monotonic_clock(0), DEQUEUED_AT_NS),
            DEQUEUED_AT_NS
        );
        assert_eq!(resolver.future_capture_stamps_clamped_to_dequeue(), 0);
    }

    #[test]
    fn a_stamp_off_the_monotonic_clock_falls_back_to_the_dequeue_instant() {
        let resolver = VideoCaptureInstantResolver::for_device("a camera");
        assert_eq!(
            resolver.resolve_capture_timestamp_ns(
                DeviceReportedCaptureStamp::OffTheMachineMonotonicClock,
                DEQUEUED_AT_NS
            ),
            DEQUEUED_AT_NS
        );
        assert_eq!(resolver.future_capture_stamps_clamped_to_dequeue(), 0);
    }

    #[test]
    fn a_device_that_never_stamps_usably_is_reported_once_however_many_frames_it_sends() {
        let resolver = VideoCaptureInstantResolver::for_device("a camera");
        let warnings = warnings_logged_while(|| {
            for _ in 0..100 {
                resolver.resolve_capture_timestamp_ns(
                    DeviceReportedCaptureStamp::OffTheMachineMonotonicClock,
                    DEQUEUED_AT_NS,
                );
                resolver.resolve_capture_timestamp_ns(on_the_monotonic_clock(0), DEQUEUED_AT_NS);
            }
        });
        assert_eq!(warnings, 1);
    }

    #[test]
    fn every_future_stamp_is_counted_but_only_the_first_is_reported() {
        let resolver = VideoCaptureInstantResolver::for_device("vivid");
        let warnings = warnings_logged_while(|| {
            for _ in 0..100 {
                resolver.resolve_capture_timestamp_ns(
                    on_the_monotonic_clock(DEQUEUED_AT_NS + 1),
                    DEQUEUED_AT_NS,
                );
            }
        });
        assert_eq!(warnings, 1);
        assert_eq!(resolver.future_capture_stamps_clamped_to_dequeue(), 100);
    }
}
