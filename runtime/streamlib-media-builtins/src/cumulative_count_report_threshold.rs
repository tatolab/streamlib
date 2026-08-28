// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How often a rising loss counter is spoken about.
//!
//! Sibling to [`crate::consecutive_failure_report_schedule`], and the
//! distinction is what each one counts. That one folds in every attempt and
//! knows a success ended a run. This one is handed a counter that only ever
//! rises — blocks dropped at a device edge, bytes of silence a device was
//! given — and answers whether the count has climbed far enough since the last
//! report to be worth another.
//!
//! Both exist because an audio built-in loses at device cadence or not at all:
//! reporting each loss buries the reason under the symptom, and reporting only
//! the first hides a fault that never went away.

/// A rising count, reported the first time it moves and then every `step`.
#[derive(Debug)]
pub struct CumulativeCountReportThreshold {
    report_at: u64,
    step: u64,
}

impl CumulativeCountReportThreshold {
    /// A threshold that speaks at the first loss and then every `step` after
    /// it. Zero would report every increment, so one is the floor.
    pub fn reporting_every(step: u64) -> Self {
        Self {
            report_at: 1,
            step: step.max(1),
        }
    }

    /// Whether `count` has risen far enough to be worth reporting, arming the
    /// next threshold when it has.
    pub fn count_is_worth_reporting(&mut self, count: u64) -> bool {
        if count < self.report_at {
            return false;
        }
        // Armed from the count rather than from the last threshold, so a run
        // that jumps by thousands reports once and not once per step it
        // skipped over.
        self.report_at = count.saturating_add(self.step);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP: u64 = 300;

    #[test]
    fn a_count_that_has_not_moved_is_not_worth_reporting() {
        let mut threshold = CumulativeCountReportThreshold::reporting_every(STEP);
        assert!(!threshold.count_is_worth_reporting(0));
    }

    #[test]
    fn the_first_loss_is_reported_and_the_next_few_are_not() {
        let mut threshold = CumulativeCountReportThreshold::reporting_every(STEP);
        assert!(threshold.count_is_worth_reporting(1));
        for count in 2..STEP {
            assert!(
                !threshold.count_is_worth_reporting(count),
                "reported again at {count}, inside the step it just armed"
            );
        }
        assert!(threshold.count_is_worth_reporting(STEP + 1));
    }

    /// A sustained loss says so periodically rather than once per unit — the
    /// same rule the consecutive-failure schedule keeps, on a counter that
    /// cannot be reset by a success.
    #[test]
    fn a_thousand_losses_are_reported_a_handful_of_times() {
        let mut threshold = CumulativeCountReportThreshold::reporting_every(STEP);
        let reported = (1..=1000)
            .filter(|count| threshold.count_is_worth_reporting(*count))
            .count();
        assert_eq!(reported, 4, "the first, then every {STEP}th");
    }

    /// The step is armed from the count, so a burst is one line rather than
    /// one line per step it jumped over.
    #[test]
    fn a_count_that_leaps_past_many_steps_reports_once_for_the_leap() {
        let mut threshold = CumulativeCountReportThreshold::reporting_every(STEP);
        assert!(threshold.count_is_worth_reporting(100_000));
        assert!(!threshold.count_is_worth_reporting(100_001));
        assert!(threshold.count_is_worth_reporting(100_000 + STEP));
    }

    /// A step of zero would report every increment, which is the shape this
    /// type exists to prevent — so it is clamped rather than trusted.
    #[test]
    fn a_step_of_zero_still_reports_periodically_rather_than_every_time() {
        let mut threshold = CumulativeCountReportThreshold::reporting_every(0);
        assert!(threshold.count_is_worth_reporting(1));
        assert!(threshold.count_is_worth_reporting(2));
    }
}
