// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One `moq-relay` child, kept running by a supervisor thread of `tatolabd`
//! until it is stopped.
//!
//! The child gets the preparation every child of the runtime gets — its own
//! process group, a lifetime bound to `tatolabd`'s where the platform can, no
//! descriptor beyond its standard streams — and an empty environment, so no
//! `MOQ_*` variable `tatolabd` inherited overrides its configuration file. Its
//! process id is written to a pidfile, and a child a crashed `tatolabd` left
//! behind is found by that file and ended before a new one starts, so its
//! port is never left bound. A child that exits is started again after a
//! backoff.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use streamlib::sdk::child_process_spawn_preparation::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use streamlib_relay_adapter_sdk::{RelayConnectionState, RelayInstanceId};

/// The first wait before a child that exited is started again.
const FIRST_RESTART_BACKOFF: Duration = Duration::from_secs(1);

/// The longest wait before a child that keeps exiting is started again.
const LONGEST_RESTART_BACKOFF: Duration = Duration::from_secs(30);

/// How long a child must run for its next exit to be met with the first
/// backoff again.
const RUN_THAT_RESETS_THE_BACKOFF: Duration = Duration::from_secs(60);

/// How long a stopped child has to end on `SIGTERM` before it is killed.
const STOPPED_CHILD_GRACE: Duration = Duration::from_secs(3);

/// How long a child a previous run left behind has to disappear once killed.
const LEFT_BEHIND_CHILD_DISAPPEARANCE_BUDGET: Duration = Duration::from_secs(5);

/// How often a disappearance or an exit is looked for.
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The suffix Linux puts on `/proc/<pid>/exe` once the executable it names has
/// been replaced on disk.
#[cfg(target_os = "linux")]
const REPLACED_EXECUTABLE_SUFFIX: &str = " (deleted)";

/// What one child is started from.
#[derive(Debug, Clone)]
pub struct LocalRelayChildLaunch {
    /// The relay instance the child is.
    pub relay_id: RelayInstanceId,
    /// The `moq-relay` executable, canonical.
    pub relay_executable: PathBuf,
    /// The configuration file the child is started with.
    pub configuration_path: PathBuf,
    /// Where the running child's process id is written.
    pub pidfile_path: PathBuf,
}

/// The thread keeping one child running, and the means to stop it.
pub struct LocalRelayChildSupervisor {
    relay_id: RelayInstanceId,
    stopping: Arc<AtomicBool>,
    running_child_process_id: Arc<Mutex<Option<u32>>>,
    wake_the_supervisor: Option<mpsc::Sender<()>>,
    supervisor_thread: Option<JoinHandle<()>>,
}

impl LocalRelayChildSupervisor {
    /// End a child a previous run left behind, then start the child and keep
    /// it running, reporting each change to `connection_state`.
    pub fn start(
        launch: LocalRelayChildLaunch,
        connection_state: tokio::sync::watch::Sender<RelayConnectionState>,
    ) -> std::io::Result<Self> {
        end_the_child_a_previous_run_left_behind(&launch.pidfile_path, &launch.relay_executable);
        let stopping = Arc::new(AtomicBool::new(false));
        let running_child_process_id = Arc::new(Mutex::new(None));
        let (wake_the_supervisor, woken) = mpsc::channel();
        let relay_id = launch.relay_id.clone();
        let supervisor_thread = std::thread::Builder::new()
            .name(format!("{}-supervisor", launch.relay_id))
            .spawn({
                let stopping = Arc::clone(&stopping);
                let running_child_process_id = Arc::clone(&running_child_process_id);
                move || {
                    keep_the_child_running(
                        &launch,
                        &stopping,
                        &running_child_process_id,
                        &woken,
                        &connection_state,
                    );
                    let _ = std::fs::remove_file(&launch.pidfile_path);
                    connection_state.send_replace(RelayConnectionState::NotJoined);
                }
            })?;
        Ok(Self {
            relay_id,
            stopping,
            running_child_process_id,
            wake_the_supervisor: Some(wake_the_supervisor),
            supervisor_thread: Some(supervisor_thread),
        })
    }

    /// The running child's process id, `None` between children.
    pub fn running_child_process_id(&self) -> Option<u32> {
        *lock_ignoring_poison(&self.running_child_process_id)
    }

    /// Stop the child — `SIGTERM`, then `SIGKILL` once its grace runs out —
    /// and the thread with it, and remove the pidfile.
    pub fn stop(&mut self) {
        let Some(supervisor_thread) = self.supervisor_thread.take() else {
            return;
        };
        self.stopping.store(true, Ordering::SeqCst);
        drop(self.wake_the_supervisor.take());
        if let Some(process_id) = self.running_child_process_id() {
            signal_the_process(process_id, libc::SIGTERM);
            let grace_ends = Instant::now() + STOPPED_CHILD_GRACE;
            while self.running_child_process_id() == Some(process_id) {
                if Instant::now() >= grace_ends {
                    tracing::warn!(
                        relay = %self.relay_id,
                        "the local relay child {process_id} outlived its {STOPPED_CHILD_GRACE:?} \
                         grace after SIGTERM and is killed"
                    );
                    signal_the_process(process_id, libc::SIGKILL);
                    break;
                }
                std::thread::sleep(PROCESS_POLL_INTERVAL);
            }
        }
        if supervisor_thread.join().is_err() {
            tracing::error!(relay = %self.relay_id, "the local relay's supervisor thread panicked");
        }
    }
}

impl Drop for LocalRelayChildSupervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn keep_the_child_running(
    launch: &LocalRelayChildLaunch,
    stopping: &AtomicBool,
    running_child_process_id: &Mutex<Option<u32>>,
    woken: &mpsc::Receiver<()>,
    connection_state: &tokio::sync::watch::Sender<RelayConnectionState>,
) {
    let mut restart_backoff = FIRST_RESTART_BACKOFF;
    loop {
        let why_the_child_is_not_running = {
            let mut running = lock_ignoring_poison(running_child_process_id);
            if stopping.load(Ordering::SeqCst) {
                return;
            }
            match start_the_child(launch) {
                Ok(child) => {
                    *running = Some(child.id());
                    drop(running);
                    write_the_pidfile(&launch.pidfile_path, child.id());
                    connection_state.send_replace(RelayConnectionState::Joined);
                    tracing::info!(
                        relay = %launch.relay_id,
                        "the local relay child {} is running from {}",
                        child.id(),
                        launch.configuration_path.display()
                    );
                    let started = Instant::now();
                    let exit = wait_for_the_child(child);
                    *lock_ignoring_poison(running_child_process_id) = None;
                    if stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    if started.elapsed() >= RUN_THAT_RESETS_THE_BACKOFF {
                        restart_backoff = FIRST_RESTART_BACKOFF;
                    }
                    match exit {
                        Ok(status) => format!("it exited with {status}"),
                        Err(failure) => format!("it could not be waited on: {failure}"),
                    }
                }
                Err(failure) => format!(
                    "{} could not be started: {failure}",
                    launch.relay_executable.display()
                ),
            }
        };
        tracing::warn!(
            relay = %launch.relay_id,
            "the local relay child is not running, because {why_the_child_is_not_running}; it \
             starts again in {restart_backoff:?}"
        );
        connection_state.send_replace(RelayConnectionState::Lost {
            why: why_the_child_is_not_running,
            rejoining_after: restart_backoff,
        });
        match woken.recv_timeout(restart_backoff) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        restart_backoff = (restart_backoff * 2).min(LONGEST_RESTART_BACKOFF);
    }
}

fn start_the_child(launch: &LocalRelayChildLaunch) -> std::io::Result<Child> {
    let mut command = Command::new(&launch.relay_executable);
    command
        .arg(&launch.configuration_path)
        .env_clear()
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    let mut child = command.spawn()?;
    if let Some(standard_output) = child.stdout.take() {
        forward_the_childs_log_lines(launch.relay_id.clone(), standard_output);
    }
    if let Some(standard_error) = child.stderr.take() {
        forward_the_childs_log_lines(launch.relay_id.clone(), standard_error);
    }
    Ok(child)
}

fn wait_for_the_child(mut child: Child) -> std::io::Result<ExitStatus> {
    child.wait()
}

/// Forward each line the child writes into this process's log: its warnings
/// and errors at their own level, the rest at debug.
fn forward_the_childs_log_lines(relay_id: RelayInstanceId, stream: impl Read + Send + 'static) {
    let forwarded = std::thread::Builder::new()
        .name(format!("{relay_id}-log"))
        .spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                match relay_log_line_level(&line) {
                    RelayLogLineLevel::Error => tracing::error!(relay = %relay_id, "{line}"),
                    RelayLogLineLevel::Warning => tracing::warn!(relay = %relay_id, "{line}"),
                    RelayLogLineLevel::Other => tracing::debug!(relay = %relay_id, "{line}"),
                }
            }
        });
    if let Err(failure) = forwarded {
        tracing::warn!("the local relay child's log is not forwarded: {failure}");
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RelayLogLineLevel {
    Error,
    Warning,
    Other,
}

/// The level `tracing-subscriber`'s plain format writes after a line's
/// timestamp.
fn relay_log_line_level(line: &str) -> RelayLogLineLevel {
    let mut words = line.split_whitespace();
    let _timestamp = words.next();
    match words.next() {
        Some("ERROR") => RelayLogLineLevel::Error,
        Some("WARN") => RelayLogLineLevel::Warning,
        _ => RelayLogLineLevel::Other,
    }
}

fn write_the_pidfile(pidfile_path: &Path, process_id: u32) {
    if let Err(failure) = std::fs::write(pidfile_path, format!("{process_id}\n")) {
        tracing::warn!(
            "the local relay child {process_id}'s pidfile {} could not be written, so a crash \
             of this runtime may leave it running: {failure}",
            pidfile_path.display()
        );
    }
}

/// End the child whose process id `pidfile_path` holds when it is still
/// running `relay_executable`, and remove the pidfile.
fn end_the_child_a_previous_run_left_behind(pidfile_path: &Path, relay_executable: &Path) {
    let Ok(pidfile) = std::fs::read_to_string(pidfile_path) else {
        return;
    };
    let _ = std::fs::remove_file(pidfile_path);
    let Ok(process_id) = pidfile.trim().parse::<u32>() else {
        return;
    };
    if !the_process_runs_the_executable(process_id, relay_executable) {
        return;
    }
    tracing::warn!(
        "a local relay child {process_id} a previous run of this runtime left behind is ended"
    );
    signal_the_process(process_id, libc::SIGKILL);
    let disappearance_deadline = Instant::now() + LEFT_BEHIND_CHILD_DISAPPEARANCE_BUDGET;
    while the_process_exists(process_id) && Instant::now() < disappearance_deadline {
        std::thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

/// Whether process `process_id` is running `executable`, so a pidfile naming a
/// process id the system has since given another program ends nothing.
fn the_process_runs_the_executable(process_id: u32, executable: &Path) -> bool {
    the_executable_of_the_process(process_id).is_some_and(|running| running == executable)
}

#[cfg(target_os = "linux")]
fn the_executable_of_the_process(process_id: u32) -> Option<PathBuf> {
    let link = std::fs::read_link(format!("/proc/{process_id}/exe")).ok()?;
    let link = link.to_string_lossy();
    Some(PathBuf::from(
        link.strip_suffix(REPLACED_EXECUTABLE_SUFFIX)
            .unwrap_or(&link),
    ))
}

#[cfg(target_os = "macos")]
fn the_executable_of_the_process(process_id: u32) -> Option<PathBuf> {
    let mut path_buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let process_id = libc::c_int::try_from(process_id).ok()?;
    // SAFETY: the buffer is `PROC_PIDPATHINFO_MAXSIZE` long and that is the
    // size passed; `proc_pidpath` writes at most that many bytes and returns
    // how many it wrote.
    let written = unsafe {
        libc::proc_pidpath(
            process_id,
            path_buffer.as_mut_ptr().cast(),
            path_buffer.len() as u32,
        )
    };
    let written = usize::try_from(written)
        .ok()
        .filter(|written| *written > 0)?;
    path_buffer.truncate(written);
    Some(PathBuf::from(
        String::from_utf8_lossy(&path_buffer).into_owned(),
    ))
}

fn the_process_exists(process_id: u32) -> bool {
    let Ok(process_id) = libc::pid_t::try_from(process_id) else {
        return false;
    };
    // SAFETY: signal 0 delivers nothing; it only asks whether the process
    // exists.
    unsafe { libc::kill(process_id, 0) == 0 }
}

fn signal_the_process(process_id: u32, signal: libc::c_int) {
    if let Ok(process_id) = libc::pid_t::try_from(process_id) {
        // SAFETY: `kill` takes scalars and touches no memory of this process.
        unsafe { libc::kill(process_id, signal) };
    }
}

fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A copy of `sleep` under a name of its own, so a test can tell the
    /// process it started from every other `sleep` on the machine.
    fn a_stand_in_relay_executable(directory: &Path) -> PathBuf {
        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .expect("the test host has `sleep`");
        let stand_in = directory.join("moq-relay");
        std::fs::copy(sleep, &stand_in).unwrap();
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
        stand_in.canonicalize().unwrap()
    }

    fn launch_in(directory: &Path, relay_executable: PathBuf) -> LocalRelayChildLaunch {
        LocalRelayChildLaunch {
            relay_id: RelayInstanceId("local-loopback".to_string()),
            relay_executable,
            configuration_path: PathBuf::from("600"),
            pidfile_path: directory.join("moq-relay.pid"),
        }
    }

    fn wait_until(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(PROCESS_POLL_INTERVAL);
        }
        condition()
    }

    #[test]
    fn the_child_runs_under_its_pidfile_and_stopping_ends_it_and_removes_the_pidfile() {
        let directory = tempfile::tempdir().unwrap();
        let launch = launch_in(
            directory.path(),
            a_stand_in_relay_executable(directory.path()),
        );
        let (connection_state, connection_state_receiver) =
            tokio::sync::watch::channel(RelayConnectionState::Joining);
        let mut supervisor =
            LocalRelayChildSupervisor::start(launch.clone(), connection_state).unwrap();

        assert!(wait_until(Duration::from_secs(10), || {
            supervisor.running_child_process_id().is_some() && launch.pidfile_path.is_file()
        }));
        let process_id = supervisor.running_child_process_id().unwrap();
        assert_eq!(
            std::fs::read_to_string(&launch.pidfile_path)
                .unwrap()
                .trim(),
            process_id.to_string()
        );
        assert_eq!(
            *connection_state_receiver.borrow(),
            RelayConnectionState::Joined
        );

        supervisor.stop();
        assert!(!the_process_exists(process_id) || the_process_is_a_zombie(process_id));
        assert!(!launch.pidfile_path.exists());
        assert_eq!(
            *connection_state_receiver.borrow(),
            RelayConnectionState::NotJoined
        );
    }

    #[test]
    fn a_child_that_exits_is_started_again_after_a_backoff() {
        let directory = tempfile::tempdir().unwrap();
        let launch = launch_in(
            directory.path(),
            a_stand_in_relay_executable(directory.path()),
        );
        let (connection_state, mut connection_state_receiver) =
            tokio::sync::watch::channel(RelayConnectionState::Joining);
        let mut supervisor =
            LocalRelayChildSupervisor::start(launch.clone(), connection_state).unwrap();
        assert!(wait_until(Duration::from_secs(10), || supervisor
            .running_child_process_id()
            .is_some()));
        let first_process_id = supervisor.running_child_process_id().unwrap();
        connection_state_receiver.mark_unchanged();

        signal_the_process(first_process_id, libc::SIGKILL);
        assert!(wait_until(Duration::from_secs(10), || matches!(
            *connection_state_receiver.borrow(),
            RelayConnectionState::Lost { .. }
        )));
        assert!(wait_until(Duration::from_secs(10), || {
            supervisor
                .running_child_process_id()
                .is_some_and(|process_id| process_id != first_process_id)
        }));
        supervisor.stop();
    }

    #[test]
    fn a_child_a_previous_run_left_behind_is_ended_before_a_new_one_starts() {
        let directory = tempfile::tempdir().unwrap();
        let relay_executable = a_stand_in_relay_executable(directory.path());
        let launch = launch_in(directory.path(), relay_executable.clone());
        let mut left_behind = Command::new(&relay_executable).arg("600").spawn().unwrap();
        let left_behind_process_id = left_behind.id();
        std::fs::write(&launch.pidfile_path, format!("{left_behind_process_id}\n")).unwrap();
        assert!(wait_until(Duration::from_secs(10), || {
            the_process_runs_the_executable(left_behind_process_id, &relay_executable)
        }));
        // Reaped as it ends, the way the init process reaps a crashed
        // runtime's orphan, so the disappearance is seen at once.
        let reaper = std::thread::spawn(move || left_behind.wait().unwrap());

        end_the_child_a_previous_run_left_behind(&launch.pidfile_path, &relay_executable);
        let status = reaper.join().unwrap();
        assert!(
            !status.success(),
            "the left-behind child was killed: {status}"
        );
        assert!(!launch.pidfile_path.exists());
    }

    #[test]
    fn a_pidfile_naming_a_process_running_another_program_ends_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let relay_executable = a_stand_in_relay_executable(directory.path());
        let launch = launch_in(directory.path(), relay_executable.clone());
        let mut another_program = Command::new("sleep").arg("600").spawn().unwrap();
        std::fs::write(&launch.pidfile_path, format!("{}\n", another_program.id())).unwrap();

        end_the_child_a_previous_run_left_behind(&launch.pidfile_path, &relay_executable);
        assert!(the_process_exists(another_program.id()));
        another_program.kill().unwrap();
        another_program.wait().unwrap();
    }

    #[test]
    fn a_relay_log_line_keeps_its_warning_and_error_levels() {
        assert_eq!(
            relay_log_line_level(
                "2026-10-18T02:21:59.085276Z  WARN moq_sock::bind: UDP receive buffer is smaller"
            ),
            RelayLogLineLevel::Warning
        );
        assert_eq!(
            relay_log_line_level("2026-10-18T02:21:59.085276Z ERROR moq_relay: bind failed"),
            RelayLogLineLevel::Error
        );
        assert_eq!(
            relay_log_line_level("2026-10-18T02:21:59.088012Z  INFO moq_relay::cluster: up"),
            RelayLogLineLevel::Other
        );
    }

    fn the_process_is_a_zombie(process_id: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{process_id}/stat"))
            .is_ok_and(|stat| stat.split_whitespace().nth(2) == Some("Z"))
    }
}
