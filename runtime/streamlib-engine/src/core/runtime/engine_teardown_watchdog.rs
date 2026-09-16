// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The watchdog that ends a teardown hung anywhere the shutdown ladder does not
//! reach.
//!
//! `docs/plan/ARCHITECTURE.md` §Language SDKs: an engine-chosen watchdog of about
//! fifteen seconds ends a teardown hung anywhere else. It is armed when a
//! teardown starts and disarmed when that teardown is over; on expiry it logs
//! what the teardown was still waiting on and ends the process with status 124.
//! An embedding host loses its interpreter with it — the change file accepts
//! that nothing hangs the app.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// How long a teardown has before the watchdog ends the process.
///
/// Engine-chosen and not authorable. It sits above every bounded wait the
/// teardown itself runs — a helper's whole ladder, a native thread's join
/// budget, tokio's shutdown — so it fires only on a hang none of them bound.
const ENGINE_TEARDOWN_WATCHDOG_BUDGET: Duration = Duration::from_secs(15);

/// The status the watchdog ends the process with: `timeout(1)`'s, and distinct
/// from the third interrupt's 130.
pub const EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED: i32 = 124;

/// How long the watchdog waits for the note of what the teardown is waiting on.
/// The thread holding it may be the one that is stuck.
const PROGRESS_NOTE_READ_BUDGET: Duration = Duration::from_millis(100);

/// What the teardown in progress is waiting on, in words, for the watchdog to
/// report if it fires.
static WHAT_THE_ENGINE_TEARDOWN_IS_WAITING_ON: parking_lot::Mutex<String> =
    parking_lot::Mutex::new(String::new());

/// Record what the teardown in progress is waiting on now.
pub fn note_what_the_engine_teardown_is_waiting_on(what: impl Into<String>) {
    *WHAT_THE_ENGINE_TEARDOWN_IS_WAITING_ON.lock() = what.into();
}

/// A watchdog armed over one teardown. Dropping it disarms it.
#[must_use = "the watchdog is disarmed the moment this is dropped"]
pub struct ArmedEngineTeardownWatchdog {
    _disarmed_when_dropped: Option<Sender<()>>,
}

impl ArmedEngineTeardownWatchdog {
    /// Arm the watchdog over the teardown `teardown_name` names.
    pub fn arm(teardown_name: &str) -> Self {
        Self::arm_with_budget(teardown_name, ENGINE_TEARDOWN_WATCHDOG_BUDGET)
    }

    fn arm_with_budget(teardown_name: &str, budget: Duration) -> Self {
        note_what_the_engine_teardown_is_waiting_on("nothing it has noted yet");
        let (disarm_sender, disarm_receiver) = std::sync::mpsc::channel::<()>();
        let teardown_name = teardown_name.to_string();
        let watching = std::thread::Builder::new()
            .name("engine-teardown-watchdog".to_string())
            .spawn(move || watch_the_teardown(&teardown_name, budget, disarm_receiver));
        match watching {
            Ok(_detached) => Self {
                _disarmed_when_dropped: Some(disarm_sender),
            },
            Err(spawn_failure) => {
                tracing::warn!(
                    "the engine teardown watchdog could not start, so nothing bounds this \
                     teardown beyond its own budgets: {spawn_failure}"
                );
                Self {
                    _disarmed_when_dropped: None,
                }
            }
        }
    }
}

fn watch_the_teardown(teardown_name: &str, budget: Duration, disarmed: Receiver<()>) {
    // The sender is never sent on: its drop disconnects the channel, which is
    // the disarm, and only a timeout means the teardown outlived the budget.
    if let Err(RecvTimeoutError::Timeout) = disarmed.recv_timeout(budget) {
        let waiting_on = WHAT_THE_ENGINE_TEARDOWN_IS_WAITING_ON
            .try_lock_for(PROGRESS_NOTE_READ_BUDGET)
            .map(|note| note.clone())
            .unwrap_or_else(|| {
                "a note held by a thread that is itself stuck, so it could not be read".to_string()
            });
        tracing::error!(
            "{}",
            the_watchdogs_expiry_message(teardown_name, budget, &waiting_on)
        );
        crate::core::runtime::kill_every_helper_process_group_and_end_the_process_at_once(
            EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED,
        );
    }
}

fn the_watchdogs_expiry_message(teardown_name: &str, budget: Duration, waiting_on: &str) -> String {
    format!(
        "{teardown_name} did not finish within {}s; it was still waiting on {waiting_on}. \
         Ending the process with status {EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED}.",
        budget.as_secs_f64(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::rerun_this_test_in_a_child_process;

    /// Set in the child process a watchdog test re-runs itself in, naming the
    /// file the child records what it needs the parent to check.
    const WATCHDOG_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_ENGINE_TEARDOWN_WATCHDOG_CHILD_RECORD_PATH";

    const A_WATCHDOG_BUDGET_A_TEST_CAN_OUTLIVE: Duration = Duration::from_millis(300);

    /// A subscriber that writes each record to stderr as it is emitted, so the
    /// watchdog's line is on the pipe before `_exit`.
    fn log_straight_to_standard_error() {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .finish(),
        );
    }

    /// A hung teardown ends with 124, names what it waited on, and takes every
    /// helper's process group with it — a helper's descendants included, which
    /// the kernel's parent-death signal never reaches.
    #[test]
    fn a_teardown_that_outlives_the_watchdog_ends_the_process_naming_what_it_waited_on() {
        if let Some(record_path) = std::env::var_os(WATCHDOG_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE)
        {
            log_straight_to_standard_error();
            let stand_in_helper =
                crate::core::test_support::a_process_parked_in_a_process_group_of_its_own();
            std::fs::write(&record_path, stand_in_helper.id().to_string())
                .expect("the record is written");
            assert!(crate::core::runtime::register_a_helper_process_group(
                stand_in_helper.id() as i32
            ));
            let _armed = ArmedEngineTeardownWatchdog::arm_with_budget(
                "the test's teardown",
                A_WATCHDOG_BUDGET_A_TEST_CAN_OUTLIVE,
            );
            note_what_the_engine_teardown_is_waiting_on("the processor thread of HungProbe");
            std::thread::sleep(Duration::from_secs(30));
            panic!("the watchdog did not end a teardown that outlived it");
        }

        let record = tempfile::tempdir().expect("a temporary directory");
        let record_path = record.path().join("helper-process-group");
        let started = std::time::Instant::now();
        let child = rerun_this_test_in_a_child_process(
            "core::runtime::engine_teardown_watchdog::tests::a_teardown_that_outlives_the_watchdog_ends_the_process_naming_what_it_waited_on",
            WATCHDOG_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE,
            record_path.as_os_str(),
        );
        let stderr = String::from_utf8_lossy(&child.stderr);

        assert_eq!(
            child.status.code(),
            Some(EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED),
            "a hung teardown was not ended with status 124: {}\n{stderr}",
            child.status,
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the watchdog took {:?} to end a teardown it budgeted 300 ms",
            started.elapsed()
        );
        assert!(
            stderr.contains("the processor thread of HungProbe"),
            "the watchdog did not say what the teardown was waiting on:\n{stderr}"
        );
        let helper_process_group: libc::pid_t = std::fs::read_to_string(&record_path)
            .expect("the child recorded its helper's process group")
            .trim()
            .parse()
            .expect("the record is a process group id");
        assert!(
            crate::core::test_support::a_process_group_is_gone_within(
                helper_process_group,
                Duration::from_secs(5)
            ),
            "a helper's process group outlived the watchdog's exit"
        );
    }

    #[test]
    fn a_teardown_that_finishes_disarms_the_watchdog() {
        if std::env::var_os(WATCHDOG_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE).is_some() {
            let armed = ArmedEngineTeardownWatchdog::arm_with_budget(
                "the test's teardown",
                A_WATCHDOG_BUDGET_A_TEST_CAN_OUTLIVE,
            );
            drop(armed);
            std::thread::sleep(A_WATCHDOG_BUDGET_A_TEST_CAN_OUTLIVE * 3);
            return;
        }

        let child = rerun_this_test_in_a_child_process(
            "core::runtime::engine_teardown_watchdog::tests::a_teardown_that_finishes_disarms_the_watchdog",
            WATCHDOG_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE,
            std::ffi::OsStr::new("unused"),
        );
        assert!(
            child.status.success(),
            "a disarmed watchdog still ended the process: {}\n{}",
            child.status,
            String::from_utf8_lossy(&child.stderr),
        );
    }

    #[test]
    fn the_expiry_message_names_the_teardown_its_budget_and_what_it_waited_on() {
        let message = the_watchdogs_expiry_message(
            "run()'s teardown",
            Duration::from_secs(15),
            "the processor thread of Recorder (p-1)",
        );
        assert!(message.contains("run()'s teardown"), "{message}");
        assert!(message.contains("15s"), "{message}");
        assert!(
            message.contains("the processor thread of Recorder (p-1)"),
            "{message}"
        );
        assert!(message.contains("124"), "{message}");
    }
}
