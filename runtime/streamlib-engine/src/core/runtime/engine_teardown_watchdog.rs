// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The watchdogs that bound a teardown hung anywhere the shutdown ladder does
//! not reach: one per loaded stream's teardown, and one over the engine's own.
//!
//! `docs/plan/ARCHITECTURE.md` §Language SDKs and §Processor model, "Failure
//! isolation": an engine-chosen watchdog of about fifteen seconds ends a
//! teardown hung anywhere else. A stream's watchdog, on expiry, kills that
//! stream's helper process groups, abandons its threads and unloads it,
//! leaving every other stream running; the engine's own ends the process with
//! status 124. Past an engine-chosen bound of abandoned threads in one
//! process, the runtime ends with status 124 as well.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// How long a teardown has before its watchdog fires.
///
/// Engine-chosen and not authorable. It sits above every bounded wait the
/// teardown itself runs — a helper's whole ladder, a native thread's join
/// budget, tokio's shutdown — so it fires only on a hang none of them bound.
pub(crate) const ENGINE_TEARDOWN_WATCHDOG_BUDGET: Duration = Duration::from_secs(15);

/// The status the watchdog ends the process with: `timeout(1)`'s, and distinct
/// from the third interrupt's 130.
pub const EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED: i32 = 124;

/// How many threads one process may abandon — processor threads past their
/// join budget and stream teardowns past their watchdog — before the runtime
/// ends. Engine-chosen: each one holds the engine alive beneath it.
const THREADS_ABANDONED_IN_ONE_PROCESS_BEFORE_THE_RUNTIME_ENDS: usize = 32;

/// How long a watchdog waits for the note of what its teardown is waiting on.
/// The thread holding it may be the one that is stuck.
const PROGRESS_NOTE_READ_BUDGET: Duration = Duration::from_millis(100);

/// Every thread this process has abandoned, counted as each is abandoned.
static THREADS_ABANDONED_IN_THIS_PROCESS: AtomicUsize = AtomicUsize::new(0);

/// Each stream a thread was abandoned for, by its cast name, which the end
/// past the bound pins the runtime's crash on.
static STREAMS_WHOSE_THREADS_WERE_ABANDONED: parking_lot::Mutex<Vec<String>> =
    parking_lot::Mutex::new(Vec::new());

/// What the engine's own teardown is waiting on, in words, for its watchdog to
/// report if it fires.
static WHAT_THE_ENGINE_TEARDOWN_IS_WAITING_ON: parking_lot::Mutex<String> =
    parking_lot::Mutex::new(String::new());

/// Record what the engine's own teardown is waiting on now.
pub fn note_what_the_engine_teardown_is_waiting_on(what: impl Into<String>) {
    *WHAT_THE_ENGINE_TEARDOWN_IS_WAITING_ON.lock() = what.into();
}

/// Count `abandoned_thread_count` threads `whose` teardown abandoned for the
/// stream `stream_name` into the process-wide total, ending the runtime with
/// status 124 once it passes the engine's bound — a crash pinned on every
/// stream a thread was abandoned for.
pub(crate) fn count_threads_abandoned_in_this_process(
    abandoned_thread_count: usize,
    whose: &str,
    stream_name: &str,
) {
    if abandoned_thread_count == 0 {
        return;
    }
    {
        let mut streams_whose_threads_were_abandoned = STREAMS_WHOSE_THREADS_WERE_ABANDONED.lock();
        if !streams_whose_threads_were_abandoned
            .iter()
            .any(|abandoned_for| abandoned_for == stream_name)
        {
            streams_whose_threads_were_abandoned.push(stream_name.to_string());
        }
    }
    let abandoned_so_far = THREADS_ABANDONED_IN_THIS_PROCESS
        .fetch_add(abandoned_thread_count, Ordering::SeqCst)
        + abandoned_thread_count;
    if abandoned_so_far > THREADS_ABANDONED_IN_ONE_PROCESS_BEFORE_THE_RUNTIME_ENDS {
        tracing::error!(
            "{whose} abandoned {abandoned_thread_count} more thread(s), so this process has \
             abandoned {abandoned_so_far}, past the engine's bound of \
             {THREADS_ABANDONED_IN_ONE_PROCESS_BEFORE_THE_RUNTIME_ENDS}. Ending the process with \
             status {EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED}."
        );
        crate::core::runtime::pin_the_runtimes_crash_on_each_stream(
            &STREAMS_WHOSE_THREADS_WERE_ABANDONED.lock(),
            &format!(
                "exit {EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED}: past the engine's bound of \
                 {THREADS_ABANDONED_IN_ONE_PROCESS_BEFORE_THE_RUNTIME_ENDS} abandoned threads, \
                 its threads among them"
            ),
        );
        crate::core::runtime::kill_every_helper_process_group_and_end_the_process_at_once(
            EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED,
        );
    }
}

/// What one loaded stream's teardown is waiting on, in words, for that
/// stream's watchdog to report if it fires.
#[derive(Debug, Clone, Default)]
pub struct TeardownProgressNoteOfOneStream {
    what_the_teardown_is_waiting_on: Arc<parking_lot::Mutex<String>>,
}

impl TeardownProgressNoteOfOneStream {
    /// Record what this stream's teardown is waiting on now.
    pub fn note_what_the_teardown_is_waiting_on(&self, what: impl Into<String>) {
        *self.what_the_teardown_is_waiting_on.lock() = what.into();
    }

    fn read_without_waiting_on_a_stuck_holder(&self) -> String {
        read_a_progress_note_without_waiting_on_a_stuck_holder(
            &self.what_the_teardown_is_waiting_on,
        )
    }
}

fn read_a_progress_note_without_waiting_on_a_stuck_holder(
    note: &parking_lot::Mutex<String>,
) -> String {
    note.try_lock_for(PROGRESS_NOTE_READ_BUDGET)
        .map(|note| note.clone())
        .unwrap_or_else(|| {
            "a note held by a thread that is itself stuck, so it could not be read".to_string()
        })
}

/// A watchdog armed over the engine's own teardown. Dropping it disarms it.
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
        let teardown_name = teardown_name.to_string();
        Self {
            _disarmed_when_dropped: spawn_a_watchdog_thread(
                "engine-teardown-watchdog",
                budget,
                move || {
                    let waiting_on = read_a_progress_note_without_waiting_on_a_stuck_holder(
                        &WHAT_THE_ENGINE_TEARDOWN_IS_WAITING_ON,
                    );
                    tracing::error!(
                        "{}",
                        the_watchdogs_expiry_message(&teardown_name, budget, &waiting_on)
                    );
                    crate::core::runtime::pin_the_runtimes_crash_on_no_stream(&format!(
                        "exit {EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED}: {teardown_name} \
                         outlived its watchdog, waiting on {waiting_on}"
                    ));
                    crate::core::runtime::kill_every_helper_process_group_and_end_the_process_at_once(
                        EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED,
                    );
                },
            ),
        }
    }
}

/// A watchdog armed over one loaded stream's teardown. Dropping it disarms it.
#[must_use = "the watchdog is disarmed the moment this is dropped"]
pub(crate) struct ArmedTeardownWatchdogOfOneStream {
    _disarmed_when_dropped: Option<Sender<()>>,
}

impl ArmedTeardownWatchdogOfOneStream {
    /// Arm `stream_name`'s watchdog for `budget`; on expiry it hands
    /// `abandon_the_stream` what the stream's teardown was still waiting on.
    pub(crate) fn arm(
        stream_name: &str,
        budget: Duration,
        teardown_progress_note: TeardownProgressNoteOfOneStream,
        abandon_the_stream: impl FnOnce(String) + Send + 'static,
    ) -> Self {
        teardown_progress_note.note_what_the_teardown_is_waiting_on("nothing it has noted yet");
        Self {
            _disarmed_when_dropped: spawn_a_watchdog_thread(
                &format!("stream-watchdog-{stream_name}"),
                budget,
                move || {
                    abandon_the_stream(
                        teardown_progress_note.read_without_waiting_on_a_stuck_holder(),
                    )
                },
            ),
        }
    }
}

/// Start a thread that runs `on_expiry` unless the returned sender drops
/// within `budget`. `None` when the thread could not start, which leaves the
/// teardown bounded by its own budgets alone.
fn spawn_a_watchdog_thread(
    thread_name: &str,
    budget: Duration,
    on_expiry: impl FnOnce() + Send + 'static,
) -> Option<Sender<()>> {
    let (disarm_sender, disarm_receiver) = std::sync::mpsc::channel::<()>();
    let watching = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || watch_the_teardown(budget, disarm_receiver, on_expiry));
    match watching {
        Ok(_detached) => Some(disarm_sender),
        Err(spawn_failure) => {
            tracing::warn!(
                "the watchdog `{thread_name}` could not start, so nothing bounds this teardown \
                 beyond its own budgets: {spawn_failure}"
            );
            None
        }
    }
}

fn watch_the_teardown(budget: Duration, disarmed: Receiver<()>, on_expiry: impl FnOnce()) {
    // The sender is never sent on: its drop disconnects the channel, which is
    // the disarm, and only a timeout means the teardown outlived the budget.
    if let Err(RecvTimeoutError::Timeout) = disarmed.recv_timeout(budget) {
        on_expiry();
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
    use std::path::Path;

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
            let (_this_run_until_the_process_ends, _) =
                crate::core::runtime::RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(
                    &the_run_in_progress_record_beside(Path::new(&record_path)),
                )
                .expect("the run's record begins");
            let stand_in_helper =
                crate::core::test_support::a_process_parked_in_a_process_group_of_its_own();
            std::fs::write(&record_path, stand_in_helper.id().to_string())
                .expect("the record is written");
            assert!(crate::core::runtime::register_a_helper_process_group(
                stand_in_helper.id() as i32,
                crate::core::runtime::LoadedStreamTag::next_in_this_process()
                    .expect("a fresh stream tag")
            ));
            let _armed = ArmedEngineTeardownWatchdog::arm_with_budget(
                "the test's teardown",
                A_WATCHDOG_BUDGET_A_TEST_CAN_OUTLIVE,
            );
            note_what_the_engine_teardown_is_waiting_on("the processor thread of HungProbe");
            std::thread::sleep(Duration::from_secs(30));
            panic!("the watchdog did not end a teardown that outlived it");
        }

        let record = crate::core::test_support::a_temporary_directory_at_owner_only_mode()
            .expect("a temporary directory");
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
        let run_in_progress_record =
            std::fs::read_to_string(the_run_in_progress_record_beside(&record_path))
                .expect("the run's record is left behind");
        assert!(
            run_in_progress_record.starts_with(
                "\texit 124: the test's teardown outlived its watchdog, waiting on the processor \
                 thread of HungProbe"
            ),
            "the watchdog's end was not pinned on no stream: {run_in_progress_record:?}"
        );
        assert_eq!(
            run_in_progress_record.lines().count(),
            1,
            "{run_in_progress_record:?}"
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

    /// Crossing the process-wide bound of abandoned threads ends the runtime
    /// with 124, takes every helper's process group with it, and pins the
    /// crash on every stream a thread was abandoned for.
    #[test]
    fn abandoning_threads_past_the_engines_bound_ends_the_process() {
        if let Some(record_path) = std::env::var_os(WATCHDOG_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE)
        {
            log_straight_to_standard_error();
            let (_this_run_until_the_process_ends, _) =
                crate::core::runtime::RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(
                &the_run_in_progress_record_beside(Path::new(&record_path)),
            )
            .expect("the run's record begins");
            let stand_in_helper =
                crate::core::test_support::a_process_parked_in_a_process_group_of_its_own();
            std::fs::write(&record_path, stand_in_helper.id().to_string())
                .expect("the record is written");
            assert!(crate::core::runtime::register_a_helper_process_group(
                stand_in_helper.id() as i32,
                crate::core::runtime::LoadedStreamTag::next_in_this_process()
                    .expect("a fresh stream tag")
            ));
            count_threads_abandoned_in_this_process(
                THREADS_ABANDONED_IN_ONE_PROCESS_BEFORE_THE_RUNTIME_ENDS,
                "the stream `at-the-bound`",
                "at-the-bound",
            );
            count_threads_abandoned_in_this_process(
                1,
                "the stream `past-the-bound`",
                "past-the-bound",
            );
            std::thread::sleep(Duration::from_secs(30));
            panic!("abandoning threads past the engine's bound did not end the process");
        }

        let record = crate::core::test_support::a_temporary_directory_at_owner_only_mode()
            .expect("a temporary directory");
        let record_path = record.path().join("helper-process-group");
        let child = rerun_this_test_in_a_child_process(
            "core::runtime::engine_teardown_watchdog::tests::abandoning_threads_past_the_engines_bound_ends_the_process",
            WATCHDOG_CHILD_RECORD_PATH_ENVIRONMENT_VARIABLE,
            record_path.as_os_str(),
        );
        let stderr = String::from_utf8_lossy(&child.stderr);

        assert_eq!(
            child.status.code(),
            Some(EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED),
            "abandoning threads past the bound did not end the process with 124: {}\n{stderr}",
            child.status,
        );
        assert!(
            stderr.contains("the stream `past-the-bound`"),
            "the end did not name whose abandoned thread crossed the bound:\n{stderr}"
        );
        assert!(
            !stderr.contains("the stream `at-the-bound`"),
            "the process ended at the bound rather than past it:\n{stderr}"
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
            "a helper's process group outlived the end past the abandoned-thread bound"
        );
        let run_in_progress_record =
            std::fs::read_to_string(the_run_in_progress_record_beside(&record_path))
                .expect("the run's record is left behind");
        let pinned_streams: Vec<&str> = run_in_progress_record
            .lines()
            .map(|line| line.split('\t').next().unwrap_or_default())
            .collect();
        assert_eq!(
            pinned_streams,
            ["at-the-bound", "past-the-bound"],
            "{run_in_progress_record}"
        );
        assert!(
            run_in_progress_record.contains("exit 124"),
            "{run_in_progress_record}"
        );
    }

    /// The run-in-progress record a watchdog child writes beside its record.
    fn the_run_in_progress_record_beside(record_path: &Path) -> std::path::PathBuf {
        record_path
            .parent()
            .expect("the record sits in a directory")
            .join("runtime-run-in-progress")
    }
}
