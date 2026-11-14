// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::os::unix::fs::PermissionsExt;

/// A copy of `sleep` under a name of its own, so a test can tell the
/// process it started from every other `sleep` on the machine.
fn a_stand_in_child_executable(directory: &Path) -> PathBuf {
    let sleep = ["/bin/sleep", "/usr/bin/sleep"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("the test host has `sleep`");
    let stand_in = directory.join("stand-in-service");
    std::fs::copy(sleep, &stand_in).unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    stand_in.canonicalize().unwrap()
}

/// Another test thread forking while the copy was open for writing holds
/// it busy for an instant, and exec fails with `ETXTBSY` until it lets go.
fn spawn_retrying_while_the_copy_is_busy(executable: &Path) -> Child {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Command::new(executable).arg("600").spawn() {
            Err(failure)
                if failure.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(CHILD_PROCESS_POLL_INTERVAL);
            }
            spawned => return spawned.unwrap(),
        }
    }
}

fn launch_in(
    directory: &Path,
    executable: PathBuf,
    gives_up_at_the_crash_loop_limit: bool,
) -> SupervisedChildProcessLaunch {
    SupervisedChildProcessLaunch {
        child_name_for_the_log: "the stand-in service".to_string(),
        executable,
        arguments: vec!["600".into()],
        environment: Vec::new(),
        pidfile_path: directory.join("stand-in-service.pid"),
        gives_up_at_the_crash_loop_limit,
        grace_after_sigterm: Duration::from_secs(3),
    }
}

fn events_into(
    events: &Arc<Mutex<Vec<SupervisedChildProcessEvent>>>,
) -> SupervisedChildProcessEventSink {
    let events = Arc::clone(events);
    Box::new(move |event| events.lock().unwrap().push(event))
}

fn wait_until(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(CHILD_PROCESS_POLL_INTERVAL);
    }
    condition()
}

#[test]
fn the_child_runs_under_its_pidfile_and_stopping_ends_it_and_removes_the_pidfile() {
    let directory = tempfile::tempdir().unwrap();
    let launch = launch_in(
        directory.path(),
        a_stand_in_child_executable(directory.path()),
        false,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut supervisor =
        SupervisedChildProcess::start(launch.clone(), Box::new(|| true), events_into(&events))
            .unwrap();

    assert!(wait_until(Duration::from_secs(10), || {
        supervisor.running_child_process_id().is_some() && launch.pidfile_path.is_file()
    }));
    let process_id = supervisor.running_child_process_id().unwrap();
    assert_eq!(
        std::fs::read_to_string(&launch.pidfile_path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        process_id.to_string()
    );
    assert!(wait_until(Duration::from_secs(10), || events
        .lock()
        .unwrap()
        .contains(&SupervisedChildProcessEvent::Ready)));

    supervisor.stop();
    assert!(!the_process_exists(process_id) || the_process_is_a_zombie(process_id));
    assert!(!launch.pidfile_path.exists());
    assert_eq!(
        *events.lock().unwrap(),
        [
            SupervisedChildProcessEvent::Started { process_id },
            SupervisedChildProcessEvent::Ready,
            SupervisedChildProcessEvent::Stopped,
        ]
    );
}

#[test]
fn stopping_keeps_a_pidfile_a_successor_rewrote() {
    let directory = tempfile::tempdir().unwrap();
    let launch = launch_in(
        directory.path(),
        a_stand_in_child_executable(directory.path()),
        false,
    );
    let mut supervisor =
        SupervisedChildProcess::start(launch.clone(), Box::new(|| true), Box::new(|_| {})).unwrap();
    assert!(wait_until(Duration::from_secs(10), || {
        supervisor.running_child_process_id().is_some() && launch.pidfile_path.is_file()
    }));
    let successor_process_id = supervisor.running_child_process_id().unwrap() + 1;
    std::fs::write(&launch.pidfile_path, format!("{successor_process_id}\n")).unwrap();

    supervisor.stop();

    assert_eq!(
        std::fs::read_to_string(&launch.pidfile_path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        successor_process_id.to_string()
    );
}

#[test]
fn a_second_supervisor_of_the_same_child_is_refused_and_the_first_child_keeps_running() {
    let directory = tempfile::tempdir().unwrap();
    let launch = launch_in(
        directory.path(),
        a_stand_in_child_executable(directory.path()),
        false,
    );
    let mut first =
        SupervisedChildProcess::start(launch.clone(), Box::new(|| true), Box::new(|_| {})).unwrap();
    assert!(wait_until(Duration::from_secs(10), || first
        .running_child_process_id()
        .is_some()));
    let first_child_process_id = first.running_child_process_id().unwrap();

    let second = SupervisedChildProcess::start(launch.clone(), Box::new(|| true), Box::new(|_| {}));

    let refusal = second.err().unwrap();
    assert_eq!(refusal.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        first.running_child_process_id(),
        Some(first_child_process_id)
    );
    assert!(the_process_exists(first_child_process_id));
    first.stop();
    drop(first);
    let mut after_the_first =
        SupervisedChildProcess::start(launch, Box::new(|| true), Box::new(|_| {})).unwrap();
    after_the_first.stop();
}

#[test]
fn a_child_that_never_passes_its_probe_is_never_reported_ready() {
    let directory = tempfile::tempdir().unwrap();
    let launch = launch_in(
        directory.path(),
        a_stand_in_child_executable(directory.path()),
        false,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut supervisor =
        SupervisedChildProcess::start(launch, Box::new(|| false), events_into(&events)).unwrap();
    assert!(wait_until(Duration::from_secs(10), || supervisor
        .running_child_process_id()
        .is_some()));
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !events
            .lock()
            .unwrap()
            .contains(&SupervisedChildProcessEvent::Ready)
    );
    supervisor.stop();
}

#[test]
fn a_child_that_exits_is_started_again_after_a_backoff() {
    let directory = tempfile::tempdir().unwrap();
    let launch = launch_in(
        directory.path(),
        a_stand_in_child_executable(directory.path()),
        false,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut supervisor =
        SupervisedChildProcess::start(launch, Box::new(|| true), events_into(&events)).unwrap();
    assert!(wait_until(Duration::from_secs(10), || supervisor
        .running_child_process_id()
        .is_some()));
    let first_process_id = supervisor.running_child_process_id().unwrap();

    signal_the_process(first_process_id, libc::SIGKILL);
    assert!(wait_until(Duration::from_secs(10), || events
        .lock()
        .unwrap()
        .iter()
        .any(|event| matches!(
            event,
            SupervisedChildProcessEvent::NotRunning { .. }
        ))));
    assert!(wait_until(Duration::from_secs(10), || {
        supervisor
            .running_child_process_id()
            .is_some_and(|process_id| process_id != first_process_id)
    }));
    supervisor.stop();
}

#[test]
fn a_child_running_user_code_that_cannot_start_is_given_up_on_at_the_crash_loop_limit() {
    let directory = tempfile::tempdir().unwrap();
    let launch = launch_in(
        directory.path(),
        directory.path().join("no-such-executable"),
        true,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut supervisor =
        SupervisedChildProcess::start(launch, Box::new(|| true), events_into(&events)).unwrap();

    // Five failed starts wait 1 + 2 + 4 + 8 seconds between them.
    assert!(wait_until(Duration::from_secs(25), || events
        .lock()
        .unwrap()
        .iter()
        .any(|event| matches!(
            event,
            SupervisedChildProcessEvent::GivenUp { .. }
        ))));
    let events = events.lock().unwrap().clone();
    let not_running = events
        .iter()
        .filter(|event| matches!(event, SupervisedChildProcessEvent::NotRunning { .. }))
        .count();
    assert_eq!(not_running, 4, "{events:?}");
    match events.last() {
        Some(SupervisedChildProcessEvent::GivenUp { why }) => {
            assert!(why.contains("died 5 times in a row"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    supervisor.stop();
}

#[test]
fn a_child_a_previous_run_left_behind_is_ended_before_a_new_one_starts() {
    let directory = tempfile::tempdir().unwrap();
    let executable = a_stand_in_child_executable(directory.path());
    let launch = launch_in(directory.path(), executable.clone(), false);
    let mut left_behind = spawn_retrying_while_the_copy_is_busy(&executable);
    let left_behind_process_id = left_behind.id();
    assert!(wait_until(Duration::from_secs(10), || {
        the_process_runs_the_executable(left_behind_process_id, &executable)
    }));
    std::fs::write(
        &launch.pidfile_path,
        the_pidfile_text_of(left_behind_process_id),
    )
    .unwrap();
    // Reaped as it ends, the way the init process reaps a crashed
    // runtime's orphan, so the disappearance is seen at once.
    let reaper = std::thread::spawn(move || left_behind.wait().unwrap());

    end_the_child_a_previous_run_left_behind(&launch.pidfile_path, &executable);
    let status = reaper.join().unwrap();
    assert!(
        !status.success(),
        "the left-behind child was killed: {status}"
    );
    assert!(!launch.pidfile_path.exists());
}

#[test]
fn a_pidfile_naming_the_executable_started_at_another_time_ends_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let executable = a_stand_in_child_executable(directory.path());
    let launch = launch_in(directory.path(), executable.clone(), false);
    let mut running_the_executable = spawn_retrying_while_the_copy_is_busy(&executable);
    let process_id = running_the_executable.id();
    assert!(wait_until(Duration::from_secs(10), || {
        the_process_runs_the_executable(process_id, &executable)
    }));
    std::fs::write(&launch.pidfile_path, format!("{process_id}\n1\n")).unwrap();

    end_the_child_a_previous_run_left_behind(&launch.pidfile_path, &executable);

    assert!(the_process_exists(process_id));
    assert!(!launch.pidfile_path.exists());
    running_the_executable.kill().unwrap();
    running_the_executable.wait().unwrap();
}

#[test]
fn a_pidfile_naming_a_process_running_another_program_ends_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let executable = a_stand_in_child_executable(directory.path());
    let launch = launch_in(directory.path(), executable.clone(), false);
    let mut another_program = Command::new("sleep").arg("600").spawn().unwrap();
    std::fs::write(&launch.pidfile_path, format!("{}\n", another_program.id())).unwrap();

    end_the_child_a_previous_run_left_behind(&launch.pidfile_path, &executable);
    assert!(the_process_exists(another_program.id()));
    another_program.kill().unwrap();
    another_program.wait().unwrap();
}

#[test]
fn a_child_log_line_past_the_bound_is_cut_and_the_next_line_read_whole() {
    let mut child_output = BufReader::with_capacity(
        8,
        std::io::Cursor::new(b"0123456789abcdef\nnext\n".to_vec()),
    );
    let mut line_bytes = Vec::new();

    assert!(read_one_line_keeping_at_most(&mut child_output, &mut line_bytes, 10).unwrap());
    assert_eq!(line_bytes, b"0123456789");
    assert!(read_one_line_keeping_at_most(&mut child_output, &mut line_bytes, 10).unwrap());
    assert_eq!(line_bytes, b"next\n");
    assert!(!read_one_line_keeping_at_most(&mut child_output, &mut line_bytes, 10).unwrap());
}

#[test]
fn a_child_log_line_keeps_its_warning_and_error_levels() {
    assert_eq!(
        supervised_child_log_line_level(
            "2026-10-18T02:21:59.085276Z  WARN moq_sock::bind: UDP receive buffer is smaller"
        ),
        SupervisedChildLogLineLevel::Warning
    );
    assert_eq!(
        supervised_child_log_line_level("2026-10-18T02:21:59.085276Z ERROR moq_relay: bind failed"),
        SupervisedChildLogLineLevel::Error
    );
    assert_eq!(
        supervised_child_log_line_level("2026-10-18T02:21:59.088012Z  INFO moq_relay: up"),
        SupervisedChildLogLineLevel::Other
    );
}

#[test]
fn a_thread_name_fits_the_kernels_fifteen_bytes() {
    assert_eq!(
        supervisor_thread_name("the browser host"),
        "the-browser-hos"
    );
    assert_eq!(supervisor_thread_name(""), "child");
}

#[cfg(target_os = "linux")]
fn the_process_is_a_zombie(process_id: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{process_id}/stat"))
        .is_ok_and(|stat| stat.split_whitespace().nth(2) == Some("Z"))
}

#[cfg(not(target_os = "linux"))]
fn the_process_is_a_zombie(_process_id: u32) -> bool {
    false
}
