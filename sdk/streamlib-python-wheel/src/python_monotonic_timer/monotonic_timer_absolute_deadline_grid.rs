// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

/// Deadlines at `first_deadline_ns + k * interval_ns`, and how many of them
/// have already been reported as expirations.
#[derive(Clone, Copy, Debug)]
pub(super) struct MonotonicTimerAbsoluteDeadlineGrid {
    pub(super) first_deadline_ns: u64,
    pub(super) interval_ns: u64,
    pub(super) deadlines_reported_count: u64,
}

impl MonotonicTimerAbsoluteDeadlineGrid {
    /// Report the deadlines passed by `now_ns` and not yet reported, and
    /// answer the next unpassed deadline — always on the grid, never relative
    /// to `now_ns`, so a late wake never moves a later deadline.
    pub(super) fn take_expirations_at(&mut self, now_ns: u64) -> (u64, u64) {
        let deadlines_passed_count = match now_ns.checked_sub(self.first_deadline_ns) {
            Some(since_first_deadline_ns) => since_first_deadline_ns / self.interval_ns + 1,
            None => 0,
        };
        let expiration_count = deadlines_passed_count.saturating_sub(self.deadlines_reported_count);
        self.deadlines_reported_count = self.deadlines_reported_count.max(deadlines_passed_count);
        let next_deadline_ns = self.first_deadline_ns.saturating_add(
            self.deadlines_reported_count
                .saturating_mul(self.interval_ns),
        );
        (expiration_count, next_deadline_ns)
    }
}
