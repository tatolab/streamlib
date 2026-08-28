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

    /// Fold one attempt into the run, and say whether this one is the one to
    /// report.
    ///
    /// The reset lives here rather than at the caller's success path so that
    /// the whole rule — a success ends a run, a failure extends it, and only
    /// some failures are spoken about — is one testable thing.
    pub fn note_attempt_and_say_whether_to_report(&mut self, succeeded: bool) -> bool {
        if succeeded {
            self.consecutive_failures = 0;
            return false;
        }
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
            .filter(|_| schedule.note_attempt_and_say_whether_to_report(false))
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

        assert!(schedule.note_attempt_and_say_whether_to_report(false));
        for _ in 0..50 {
            schedule.note_attempt_and_say_whether_to_report(false);
        }
        assert_eq!(schedule.consecutive_failures(), 51);

        assert!(!schedule.note_attempt_and_say_whether_to_report(true));
        assert_eq!(schedule.consecutive_failures(), 0, "a success ends the run");

        assert!(
            schedule.note_attempt_and_say_whether_to_report(false),
            "the first failure of a new run is reported"
        );
    }

    #[test]
    fn nothing_has_failed_yet_so_there_is_nothing_to_report() {
        let mut schedule = a_schedule();
        assert!(!schedule.note_attempt_and_say_whether_to_report(true));
        assert_eq!(schedule.consecutive_failures(), 0);
    }

    /// A period of zero would report every failure, which is the shape this
    /// type exists to prevent — so it is clamped rather than trusted.
    #[test]
    fn a_period_of_zero_still_reports_periodically_rather_than_every_time() {
        let mut schedule = ConsecutiveFailureReportSchedule::reporting_every(0);
        assert!(schedule.note_attempt_and_say_whether_to_report(false));
        assert!(
            schedule.note_attempt_and_say_whether_to_report(false),
            "every failure is a multiple of one, which is the floor a zero lands on"
        );
    }
}
