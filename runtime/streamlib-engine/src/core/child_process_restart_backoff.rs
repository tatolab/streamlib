// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one restart policy every child process the runtime keeps alive runs
//! under: how long to wait before starting it again, and when a child running
//! user code has died soon after its start often enough to be given up on.

use std::time::Duration;

/// The wait before the first start after a death.
pub const FIRST_CHILD_PROCESS_RESTART_WAIT: Duration = Duration::from_secs(1);

/// The longest the wait grows to, doubling after each death.
pub const LONGEST_CHILD_PROCESS_RESTART_WAIT: Duration = Duration::from_secs(30);

/// A run at least this long counts as healthy: the wait goes back to its first
/// value and the deaths in a row go back to none.
pub const CHILD_PROCESS_RUN_THAT_RESETS_THE_RESTART_BACKOFF: Duration = Duration::from_secs(60);

/// The deaths in a row, each before a healthy run, after which a child running
/// user code is given up on.
pub const CRASH_LOOP_LIMIT_OF_A_CHILD_PROCESS_RUNNING_USER_CODE: u32 = 5;

/// One child process's restart backoff, fed each death as it happens.
#[derive(Debug, Clone)]
pub struct ChildProcessRestartBackoff {
    wait_before_the_next_start: Duration,
    deaths_in_a_row_before_a_healthy_run: u32,
}

impl Default for ChildProcessRestartBackoff {
    fn default() -> Self {
        Self::new()
    }
}

impl ChildProcessRestartBackoff {
    /// A backoff for a child that has not died yet.
    pub fn new() -> Self {
        Self {
            wait_before_the_next_start: FIRST_CHILD_PROCESS_RESTART_WAIT,
            deaths_in_a_row_before_a_healthy_run: 0,
        }
    }

    /// Record a death that ended a run of `run_before_the_death` — zero for a
    /// start that failed — and return how long to wait before starting again.
    pub fn wait_before_starting_again_after_a_death_ending_a_run_of(
        &mut self,
        run_before_the_death: Duration,
    ) -> Duration {
        if run_before_the_death >= CHILD_PROCESS_RUN_THAT_RESETS_THE_RESTART_BACKOFF {
            self.wait_before_the_next_start = FIRST_CHILD_PROCESS_RESTART_WAIT;
            self.deaths_in_a_row_before_a_healthy_run = 0;
        } else {
            self.deaths_in_a_row_before_a_healthy_run += 1;
        }
        let wait = self.wait_before_the_next_start;
        self.wait_before_the_next_start = (wait * 2).min(LONGEST_CHILD_PROCESS_RESTART_WAIT);
        wait
    }

    /// The deaths in a row so far, each before a healthy run.
    pub fn deaths_in_a_row_before_a_healthy_run(&self) -> u32 {
        self.deaths_in_a_row_before_a_healthy_run
    }

    /// Whether a child running user code has died soon after its start often
    /// enough in a row to be given up on.
    pub fn has_reached_the_crash_loop_limit_of_a_child_running_user_code(&self) -> bool {
        self.deaths_in_a_row_before_a_healthy_run
            >= CRASH_LOOP_LIMIT_OF_A_CHILD_PROCESS_RUNNING_USER_CODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_SHORT_RUN: Duration = Duration::from_millis(500);

    #[test]
    fn the_wait_starts_at_a_second_and_doubles_to_thirty() {
        let mut backoff = ChildProcessRestartBackoff::new();
        let waits: Vec<u64> = (0..7)
            .map(|_| {
                backoff
                    .wait_before_starting_again_after_a_death_ending_a_run_of(A_SHORT_RUN)
                    .as_secs()
            })
            .collect();
        assert_eq!(waits, vec![1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn a_healthy_run_resets_the_wait_and_the_deaths_in_a_row() {
        let mut backoff = ChildProcessRestartBackoff::new();
        for _ in 0..4 {
            backoff.wait_before_starting_again_after_a_death_ending_a_run_of(A_SHORT_RUN);
        }
        assert_eq!(backoff.deaths_in_a_row_before_a_healthy_run(), 4);

        let wait = backoff.wait_before_starting_again_after_a_death_ending_a_run_of(
            CHILD_PROCESS_RUN_THAT_RESETS_THE_RESTART_BACKOFF,
        );

        assert_eq!(wait, FIRST_CHILD_PROCESS_RESTART_WAIT);
        assert_eq!(backoff.deaths_in_a_row_before_a_healthy_run(), 0);
    }

    #[test]
    fn the_fifth_short_lived_death_in_a_row_reaches_the_crash_loop_limit() {
        let mut backoff = ChildProcessRestartBackoff::new();
        for _ in 0..4 {
            backoff.wait_before_starting_again_after_a_death_ending_a_run_of(A_SHORT_RUN);
            assert!(!backoff.has_reached_the_crash_loop_limit_of_a_child_running_user_code());
        }
        backoff.wait_before_starting_again_after_a_death_ending_a_run_of(Duration::ZERO);
        assert!(backoff.has_reached_the_crash_loop_limit_of_a_child_running_user_code());
    }

    #[test]
    fn a_child_that_dies_rarely_never_reaches_the_crash_loop_limit() {
        let mut backoff = ChildProcessRestartBackoff::new();
        for _ in 0..20 {
            backoff.wait_before_starting_again_after_a_death_ending_a_run_of(
                CHILD_PROCESS_RUN_THAT_RESETS_THE_RESTART_BACKOFF * 60,
            );
        }
        assert!(!backoff.has_reached_the_crash_loop_limit_of_a_child_running_user_code());
    }
}
