// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use pyo3::prelude::*;

use super::PythonMonotonicTimer;
use super::monotonic_timer_absolute_deadline_grid::MonotonicTimerAbsoluteDeadlineGrid;

const INTERVAL_NS: u64 = 5_000_000;

fn a_grid_starting_at(first_deadline_ns: u64) -> MonotonicTimerAbsoluteDeadlineGrid {
    MonotonicTimerAbsoluteDeadlineGrid {
        first_deadline_ns,
        interval_ns: INTERVAL_NS,
        deadlines_reported_count: 0,
    }
}

#[test]
fn a_wake_before_the_first_deadline_reports_nothing_and_keeps_that_deadline() {
    let mut grid = a_grid_starting_at(1_000_000_000);

    assert_eq!(grid.take_expirations_at(999_000_000), (0, 1_000_000_000));
}

#[test]
fn a_wake_on_a_deadline_reports_it_and_arms_the_next_one() {
    let mut grid = a_grid_starting_at(1_000_000_000);

    assert_eq!(
        grid.take_expirations_at(1_000_000_000),
        (1, 1_000_000_000 + INTERVAL_NS)
    );
}

/// Each wake lands late by more than the whole interval's slack a
/// relative re-arm would lose, and the deadlines still sit on the grid.
#[test]
fn late_wakes_never_move_a_later_deadline_off_the_grid() {
    const FIRST_DEADLINE_NS: u64 = 1_000_000_000;
    const LATENESS_EVERY_WAKE_NS: u64 = 4_000_000;
    let mut grid = a_grid_starting_at(FIRST_DEADLINE_NS);
    let mut armed_deadline_ns = FIRST_DEADLINE_NS;

    for period in 1..=400u64 {
        let (expiration_count, next_deadline_ns) =
            grid.take_expirations_at(armed_deadline_ns + LATENESS_EVERY_WAKE_NS);
        assert_eq!(expiration_count, 1, "period {period}");
        assert_eq!(
            next_deadline_ns,
            FIRST_DEADLINE_NS + period * INTERVAL_NS,
            "period {period}: the next deadline left the absolute grid"
        );
        armed_deadline_ns = next_deadline_ns;
    }
}

#[test]
fn a_wake_that_slept_through_deadlines_reports_each_of_them_once() {
    let mut grid = a_grid_starting_at(1_000_000_000);

    assert_eq!(
        grid.take_expirations_at(1_000_000_000 + 3 * INTERVAL_NS + 1),
        (4, 1_000_000_000 + 4 * INTERVAL_NS)
    );
    assert_eq!(
        grid.take_expirations_at(1_000_000_000 + 3 * INTERVAL_NS + 2),
        (0, 1_000_000_000 + 4 * INTERVAL_NS),
        "a second wake inside the same period must report nothing new"
    );
}

#[test]
fn a_closed_timer_reports_minus_one() {
    Python::initialize();
    Python::attach(|python| {
        let timer = PythonMonotonicTimer::new(2_000_000).unwrap();
        timer.close();
        assert_eq!(timer.wait(python, 0), -1);
    });
}

#[test]
fn a_non_positive_interval_is_refused() {
    assert!(PythonMonotonicTimer::new(0).is_err());
    assert!(PythonMonotonicTimer::new(-5).is_err());
}
