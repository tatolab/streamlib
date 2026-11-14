// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::sync::Arc;

use super::*;
use crate::core::logging::ProjectLogRouteEntry;
use crate::core::test_support::{
    a_temporary_directory_at_owner_only_mode, rerun_this_test_in_a_child_process,
};

/// Set only in the child process a test re-runs itself in, to the
/// directory its run-in-progress record sits in.
const CRASH_RECORD_CHILD_DIRECTORY_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_RUNTIME_CRASH_RECORD_CHILD_DIRECTORY";

const RECORD_FILE_NAME: &str = "runtime-run-in-progress";

/// The route a thread of the stream `stream_id` carries, writing no file.
fn the_log_route_of_the_stream(
    stream_id: &str,
    project_directory: &Path,
) -> Arc<ProjectLogRouteEntry> {
    ProjectLogRouteEntry::for_a_stream_of_the_project_in(
        project_directory,
        "R-test",
        stream_id,
        None,
    )
}

/// Re-run `test_path` in a child process holding a fresh directory for its
/// record, and hand back how it ended and what its record holds.
fn the_childs_end_and_its_record(test_path: &str) -> (std::process::Output, Option<String>) {
    let directory = a_temporary_directory_at_owner_only_mode().expect("a record directory");
    let child = rerun_this_test_in_a_child_process(
        test_path,
        CRASH_RECORD_CHILD_DIRECTORY_ENVIRONMENT_VARIABLE,
        directory.path().as_os_str(),
    );
    let record = std::fs::read_to_string(directory.path().join(RECORD_FILE_NAME)).ok();
    (child, record)
}

/// End `run` as the process's death does: nothing more is written, and
/// the record stays as it is.
fn end_as_the_process_does_on_a_crash(mut run: RuntimeRunInProgressRecord) {
    run.ended = true;
    stop_recording_this_run();
}

fn the_child_directory() -> Option<PathBuf> {
    std::env::var_os(CRASH_RECORD_CHILD_DIRECTORY_ENVIRONMENT_VARIABLE).map(PathBuf::from)
}

#[test]
fn a_record_names_each_stream_by_its_first_cause_and_keeps_each_cause_no_stream_owns() {
    let crash = CrashOfThePreviousRuntimeRun::read_from(
        b"crasher\tSIGSEGV\n\tSIGBUS\nnot a crash line\ncrasher\tSIGABRT\nother\texit 124\n",
    );

    assert_eq!(
        crash.causes_by_implicated_stream,
        BTreeMap::from([
            ("crasher".to_string(), "SIGSEGV".to_string()),
            ("other".to_string(), "exit 124".to_string()),
        ])
    );
    assert_eq!(crash.causes_on_threads_no_stream_owns, ["SIGBUS"]);
    assert!(!crash.left_no_trace());
    assert!(CrashOfThePreviousRuntimeRun::read_from(b"").left_no_trace());
}

#[test]
fn a_stream_id_past_the_longest_a_thread_carries_is_cut() {
    let long_name = "s".repeat(80);

    let carried = StreamThisThreadWorksFor::of_the_stream_id(&long_name);

    assert_eq!(
        carried.stream_id_bytes(),
        &long_name.as_bytes()[..LONGEST_STREAM_ID_A_THREAD_CARRIES_FOR_ITS_CRASH]
    );
    assert!(StreamThisThreadWorksFor::NONE.is_none());
}

/// A `SIGSEGV` on a thread carrying a stream's log route is pinned on that
/// stream, and the process still ends on the signal.
#[test]
fn a_fatal_signal_on_a_thread_working_for_a_stream_is_pinned_on_it_and_ends_the_process() {
    if let Some(directory) = the_child_directory() {
        let (_this_run_until_the_process_ends, _) =
            RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(
                &directory.join(RECORD_FILE_NAME),
            )
            .expect("the run's record begins");
        let route = the_log_route_of_the_stream("crasher", &directory);
        std::thread::spawn(move || {
            let _entered = route.enter_on_this_thread();
            // SAFETY: `raise` takes a signal number.
            unsafe { libc::raise(libc::SIGSEGV) };
        })
        .join()
        .expect("the thread runs");
        panic!("a SIGSEGV did not end the process");
    }

    let (child, record) = the_childs_end_and_its_record(
        "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_fatal_signal_on_a_thread_working_for_a_stream_is_pinned_on_it_and_ends_the_process",
    );

    assert_eq!(child.status.signal(), Some(libc::SIGSEGV), "{child:?}");
    assert_eq!(record.as_deref(), Some("crasher\tSIGSEGV\n"));
}

/// An abort on a thread no stream's route is entered on — including one
/// whose route has since been left — implicates no stream.
#[test]
fn a_fatal_signal_on_a_thread_working_for_no_stream_is_pinned_on_none() {
    if let Some(directory) = the_child_directory() {
        let (_this_run_until_the_process_ends, _) =
            RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(
                &directory.join(RECORD_FILE_NAME),
            )
            .expect("the run's record begins");
        the_log_route_of_the_stream("left-before-the-crash", &directory).run_entered(|| {});
        std::process::abort();
    }

    let (child, record) = the_childs_end_and_its_record(
        "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_fatal_signal_on_a_thread_working_for_no_stream_is_pinned_on_none",
    );

    assert_eq!(child.status.signal(), Some(libc::SIGABRT), "{child:?}");
    assert_eq!(record.as_deref(), Some("\tSIGABRT\n"));
}

/// The hook writes nothing for a panic a thread catches; a panic that
/// escapes the main thread is pinned on the stream its thread worked for,
/// never on a bystander that panicked after it.
#[test]
fn a_caught_panic_records_nothing_and_one_escaping_the_main_thread_is_pinned_on_its_stream() {
    if let Some(directory) = the_child_directory() {
        let record_path = directory.join(RECORD_FILE_NAME);
        let (_this_run_until_the_process_ends, _) =
            RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(&record_path)
                .expect("the run's record begins");
        let caught = the_log_route_of_the_stream("caught", &directory)
            .run_entered(|| std::panic::catch_unwind(|| panic!("a panic its thread catches")));
        assert!(caught.is_err());
        assert_eq!(
            std::fs::read_to_string(&record_path).expect("the record is there"),
            "",
            "a caught panic recorded something"
        );
        let escaped_panic_payload = std::panic::catch_unwind(|| {
            the_log_route_of_the_stream("escaping", &directory)
                .run_entered(|| panic!("a panic that ends the process"))
        })
        .expect_err("the panic escaped");
        let bystander_route = the_log_route_of_the_stream("bystander", &directory);
        std::thread::spawn(move || {
            bystander_route
                .run_entered(|| std::panic::catch_unwind(|| panic!("a bystander's caught panic")))
        })
        .join()
        .expect("the bystander's thread runs")
        .expect_err("the bystander panicked");
        pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread(&*escaped_panic_payload);
        std::process::exit(0);
    }

    let (child, record) = the_childs_end_and_its_record(
        "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_caught_panic_records_nothing_and_one_escaping_the_main_thread_is_pinned_on_its_stream",
    );

    assert!(child.status.success(), "{child:?}");
    let record = record.expect("the record is left behind");
    assert!(
        record.starts_with(
            "escaping\ta panic that escaped the main thread: a panic that ends the process at "
        ),
        "{record}"
    );
    assert_eq!(record.lines().count(), 1, "{record}");
}

/// A clean end removes the record, so the next start reads none; an end as
/// a crash leaves the streams it names; the owner's end at once removes it.
#[test]
fn a_clean_end_leaves_no_crash_and_a_crash_end_leaves_the_streams_it_was_pinned_on() {
    if let Some(directory) = the_child_directory() {
        let record_path = directory.join(RECORD_FILE_NAME);
        let begin = || {
            RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(&record_path)
                .expect("the run's record begins")
        };

        let (first_run, how_the_first_previous_run_ended) = begin();
        assert_eq!(
            how_the_first_previous_run_ended,
            HowThePreviousRuntimeRunEnded::Cleanly
        );
        assert!(
            RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(&record_path).is_err(),
            "a second run began while one was recorded"
        );
        first_run.end_this_run_cleanly();
        assert!(!record_path.exists());

        let (second_run, how_the_second_previous_run_ended) = begin();
        assert_eq!(
            how_the_second_previous_run_ended,
            HowThePreviousRuntimeRunEnded::Cleanly
        );
        pin_the_runtimes_crash_on_each_stream(
            &["first".to_string(), "second".to_string()],
            "exit 124:\tabandoned",
        );
        end_as_the_process_does_on_a_crash(second_run);

        let (mut third_run, how_the_third_previous_run_ended) = begin();
        assert_eq!(
            how_the_third_previous_run_ended,
            HowThePreviousRuntimeRunEnded::Crashed(CrashOfThePreviousRuntimeRun {
                causes_by_implicated_stream: BTreeMap::from([
                    ("first".to_string(), "exit 124: abandoned".to_string()),
                    ("second".to_string(), "exit 124: abandoned".to_string()),
                ]),
                causes_on_threads_no_stream_owns: Vec::new(),
            })
        );
        third_run.mark_the_previous_runs_end_counted();
        end_the_run_in_progress_record_as_the_owner_ends_the_process();
        assert!(!record_path.exists());
        end_as_the_process_does_on_a_crash(third_run);

        let (_fourth_run, how_the_fourth_previous_run_ended) = begin();
        assert_eq!(
            how_the_fourth_previous_run_ended,
            HowThePreviousRuntimeRunEnded::Cleanly
        );
        assert_eq!(
            std::fs::metadata(&record_path)
                .expect("the fourth run's record is there")
                .permissions()
                .mode()
                & 0o777,
            RUN_IN_PROGRESS_RECORD_FILE_MODE
        );
        std::process::exit(0);
    }

    let (child, record) = the_childs_end_and_its_record(
        "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_clean_end_leaves_no_crash_and_a_crash_end_leaves_the_streams_it_was_pinned_on",
    );

    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert_eq!(
        record.as_deref(),
        Some(""),
        "a run never ended reads as a crash pinned on nothing"
    );
}

/// A run dropped before it counted the previous run's crash — a start
/// refused early — puts that crash back for the next start; one dropped
/// after counting ends cleanly.
#[test]
fn a_run_dropped_before_counting_puts_the_previous_crash_back_and_after_counting_ends_cleanly() {
    if let Some(directory) = the_child_directory() {
        let record_path = directory.join(RECORD_FILE_NAME);
        std::fs::write(&record_path, "crasher\tSIGSEGV\n").unwrap();
        let begin = || {
            RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(&record_path)
                .expect("the run's record begins")
        };

        let (refused_start, _) = begin();
        drop(refused_start);
        let (mut counted_start, how_the_previous_run_ended) = begin();
        assert_eq!(
            how_the_previous_run_ended
                .crash()
                .map(|crash| crash.causes_by_implicated_stream.clone()),
            Some(BTreeMap::from([(
                "crasher".to_string(),
                "SIGSEGV".to_string()
            )])),
            "the refused start did not put the crash back"
        );
        counted_start.mark_the_previous_runs_end_counted();
        drop(counted_start);
        assert!(!record_path.exists());
        std::process::exit(0);
    }

    let (child, record) = the_childs_end_and_its_record(
        "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_run_dropped_before_counting_puts_the_previous_crash_back_and_after_counting_ends_cleanly",
    );

    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert_eq!(record, None);
}
