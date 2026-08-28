// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How often a run of identical failures is spoken about.
//!
//! An audio built-in fails at device cadence or not at all: an output port
//! nobody connected fails every block, and a speaker handed the wrong format
//! refuses every block — roughly ninety-four a second at the default quantum.
//! Reporting each one buries the rest of the log rather than telling anyone
//! anything new, and reporting only the first hides a fault that never went
//! away. So a run is reported at its first failure and then periodically, and
//! a success ends the run.

/// A run of identical failures, reported at the first and then every Nth.
#[derive(Debug)]
pub struct ConsecutiveFailureReportSchedule {
    consecutive_failures: u64,
    failures_between_reports: u64,
}

impl ConsecutiveFailureReportSchedule {
    /// A schedule that speaks at the first failure of a run and then every
    /// `failures_between_reports`. Zero would report every failure, so one is
    /// the floor.
    pub fn reporting_every(failures_between_reports: u64) -> Self {
        Self {
            consecutive_failures: 0,
            failures_between_reports: failures_between_reports.max(1),
        }
    }

    /// End the current run of failures.
    ///
    /// Named rather than folded into a boolean parameter on the failure path:
    /// `note_attempt(true)` at a call site says nothing about which of the two
    /// things it means, and this is read far more often than it is written.
    pub fn note_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Extend the run of failures, and say whether this one is the one to
    /// report.
    pub fn note_failure_and_say_whether_to_report(&mut self) -> bool {
        self.consecutive_failures += 1;
        self.consecutive_failures == 1
            || self
                .consecutive_failures
                .is_multiple_of(self.failures_between_reports)
    }

    /// How long the current run of failures is; zero when the last attempt
    /// succeeded.
    pub fn consecutive_failures(&self) -> u64 {
        self.consecutive_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAILURES_BETWEEN_REPORTS: u64 = 300;

    fn a_schedule() -> ConsecutiveFailureReportSchedule {
        ConsecutiveFailureReportSchedule::reporting_every(FAILURES_BETWEEN_REPORTS)
    }

    /// The defect this exists to prevent, held by counting rather than by
    /// re-deriving the rule: an unconnected port failed every block and
    /// reported every one, burying an observed run under 18 059 lines.
    #[test]
    fn a_thousand_failures_are_reported_a_handful_of_times() {
        let mut schedule = a_schedule();
        let reported = (0..1000)
            .filter(|_| schedule.note_failure_and_say_whether_to_report())
            .count();
        assert_eq!(
            reported, 4,
            "the first failure and every {FAILURES_BETWEEN_REPORTS}th, not one per attempt"
        );
        assert_eq!(schedule.consecutive_failures(), 1000);
    }

    /// A success ends a run, so the next failure is a first failure again — a
    /// fault that clears and returns must not go unmentioned the second time.
    #[test]
    fn a_success_ends_the_run_and_the_next_failure_is_reported_again() {
        let mut schedule = a_schedule();

        assert!(schedule.note_failure_and_say_whether_to_report());
        for _ in 0..50 {
            schedule.note_failure_and_say_whether_to_report();
        }
        assert_eq!(schedule.consecutive_failures(), 51);

        schedule.note_success();
        assert_eq!(schedule.consecutive_failures(), 0, "a success ends the run");

        assert!(
            schedule.note_failure_and_say_whether_to_report(),
            "the first failure of a new run is reported"
        );
    }

    #[test]
    fn nothing_has_failed_yet_so_there_is_nothing_to_report() {
        let mut schedule = a_schedule();
        schedule.note_success();
        assert_eq!(schedule.consecutive_failures(), 0);
    }

    /// A period of zero would report every failure, which is the shape this
    /// type exists to prevent — so it is clamped rather than trusted.
    #[test]
    fn a_period_of_zero_still_reports_periodically_rather_than_every_time() {
        let mut schedule = ConsecutiveFailureReportSchedule::reporting_every(0);
        assert!(schedule.note_failure_and_say_whether_to_report());
        assert!(
            schedule.note_failure_and_say_whether_to_report(),
            "every failure is a multiple of one, which is the floor a zero lands on"
        );
    }
}
