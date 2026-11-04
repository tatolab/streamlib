// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A long-lived native child the runtime keeps running — a local relay, a
//! shared native service — from its start until it is stopped.
//!
//! The child gets the preparation every child of the runtime gets — its own
//! process group, a lifetime bound to the runtime's where the platform can, no
//! descriptor beyond its standard streams — and an empty environment but for
//! the variables its launch names. Its process id is written to a pidfile, and
//! a child a crashed runtime left behind is found by that file and ended
//! before a new one starts. A child that exits is started again under the one
//! [`ChildProcessRestartBackoff`]; one running user code is given up on at its
//! crash-loop limit. A child is signalled only while it is unreaped, so its
//! pid cannot have been handed to another process. Each line it writes is
//! forwarded into the runtime's log.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::core::child_process_lifetime::{
    a_child_process_has_exited_without_being_reaped,
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::child_process_restart_backoff::ChildProcessRestartBackoff;

/// How long a child a previous run left behind has to disappear once killed.
const LEFT_BEHIND_CHILD_DISAPPEARANCE_BUDGET: Duration = Duration::from_secs(5);

/// How often a disappearance, a readiness or an exit is looked for.
const CHILD_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How often the supervisor looks for its running child's exit.
const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The suffix Linux puts on `/proc/<pid>/exe` once the executable it names has
/// been replaced on disk.
#[cfg(target_os = "linux")]
const REPLACED_EXECUTABLE_SUFFIX: &str = " (deleted)";

/// What one supervised child is started from.
#[derive(Debug, Clone)]
pub struct SupervisedChildProcessLaunch {
    /// What the runtime's log calls the child.
    pub child_name_for_the_log: String,
    /// The executable, canonical.
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    /// The only variables the child's environment holds.
    pub environment: Vec<(OsString, OsString)>,
    /// Where the running child's process id is written.
    pub pidfile_path: PathBuf,
    /// Whether the child is given up on at the crash-loop limit, as one
    /// running code the runtime does not ship is, rather than started again
    /// forever.
    pub gives_up_at_the_crash_loop_limit: bool,
    /// How long a stopped child has to end on `SIGTERM` before it is killed.
    pub grace_after_sigterm: Duration,
}

/// What became of a supervised child, as the supervisor reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisedChildProcessEvent {
    /// A child started and has not yet passed its readiness probe.
    Started { process_id: u32 },
    /// The running child passed its readiness probe.
    Ready,
    /// The child is not running, and starts again after the wait.
    NotRunning {
        why: String,
        starting_again_after: Duration,
    },
    /// The child ran user code and died at its crash-loop limit; it is not
    /// started again, and nothing is reported after it.
    GivenUp { why: String },
    /// The supervisor was stopped and the child with it. Last, unless the
    /// child was given up on.
    Stopped,
}

/// Whether a just-started child is ready, asked until it says so or exits.
pub type SupervisedChildProcessReadinessProbe = Box<dyn Fn() -> bool + Send>;

/// Where the supervisor reports each [`SupervisedChildProcessEvent`].
pub type SupervisedChildProcessEventSink = Box<dyn Fn(SupervisedChildProcessEvent) + Send>;

/// The thread keeping one child running, and the means to stop it.
pub struct SupervisedChildProcess {
    child_name_for_the_log: String,
    grace_after_sigterm: Duration,
    stopping: Arc<AtomicBool>,
    running_child_process_id: Arc<Mutex<Option<u32>>>,
    wake_the_supervisor: Option<mpsc::Sender<()>>,
    supervisor_thread: Option<JoinHandle<()>>,
}

impl SupervisedChildProcess {
    /// End a child a previous run left behind, then start the child and keep
    /// it running, reporting each change to `report`.
    pub fn start(
        launch: SupervisedChildProcessLaunch,
        readiness_probe: SupervisedChildProcessReadinessProbe,
        report: SupervisedChildProcessEventSink,
    ) -> std::io::Result<Self> {
        end_the_child_a_previous_run_left_behind(&launch.pidfile_path, &launch.executable);
        let stopping = Arc::new(AtomicBool::new(false));
        let running_child_process_id = Arc::new(Mutex::new(None));
        let (wake_the_supervisor, woken) = mpsc::channel();
        let child_name_for_the_log = launch.child_name_for_the_log.clone();
        let grace_after_sigterm = launch.grace_after_sigterm;
        let supervisor_thread = std::thread::Builder::new()
            .name(supervisor_thread_name(&launch.child_name_for_the_log))
            .spawn({
                let stopping = Arc::clone(&stopping);
                let running_child_process_id = Arc::clone(&running_child_process_id);
                move || {
                    let mut last_started_process_id = None;
                    let ended_by = keep_the_child_running(
                        &launch,
                        &readiness_probe,
                        &report,
                        &stopping,
                        &running_child_process_id,
                        &woken,
                        &mut last_started_process_id,
                    );
                    if let Some(process_id) = last_started_process_id {
                        remove_the_pidfile_while_it_names(&launch.pidfile_path, process_id);
                    }
                    report(ended_by);
                }
            })?;
        Ok(Self {
            child_name_for_the_log,
            grace_after_sigterm,
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
        if let Some(process_id) = self.running_child_process_id()
            && self.signal_the_child_while_unreaped(process_id, libc::SIGTERM)
        {
            let grace_ends = Instant::now() + self.grace_after_sigterm;
            while self.running_child_process_id() == Some(process_id) {
                if Instant::now() >= grace_ends {
                    if self.signal_the_child_while_unreaped(process_id, libc::SIGKILL) {
                        tracing::warn!(
                            "{} {process_id} outlived its {:?} grace after SIGTERM and is killed",
                            self.child_name_for_the_log,
                            self.grace_after_sigterm
                        );
                    }
                    break;
                }
                std::thread::sleep(CHILD_PROCESS_POLL_INTERVAL);
            }
        }
        if supervisor_thread.join().is_err() {
            tracing::error!(
                "the supervisor thread of {} panicked",
                self.child_name_for_the_log
            );
        }
    }

    /// Send `signal` to child `process_id` while it is still the running,
    /// unreaped child, so a pid the system has freed is never signalled.
    fn signal_the_child_while_unreaped(&self, process_id: u32, signal: libc::c_int) -> bool {
        let running = lock_ignoring_poison(&self.running_child_process_id);
        if *running != Some(process_id) {
            return false;
        }
        signal_the_process(process_id, signal);
        true
    }
}

impl Drop for SupervisedChildProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn supervisor_thread_name(child_name_for_the_log: &str) -> String {
    let mut name: String = child_name_for_the_log
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .take(15)
        .collect();
    if name.is_empty() {
        name.push_str("child");
    }
    name
}

fn keep_the_child_running(
    launch: &SupervisedChildProcessLaunch,
    readiness_probe: &SupervisedChildProcessReadinessProbe,
    report: &SupervisedChildProcessEventSink,
    stopping: &AtomicBool,
    running_child_process_id: &Mutex<Option<u32>>,
    woken: &mpsc::Receiver<()>,
    last_started_process_id: &mut Option<u32>,
) -> SupervisedChildProcessEvent {
    let mut restart_backoff = ChildProcessRestartBackoff::new();
    loop {
        let (why_the_child_is_not_running, run_before_the_death) = {
            let mut running = lock_ignoring_poison(running_child_process_id);
            if stopping.load(Ordering::SeqCst) {
                return SupervisedChildProcessEvent::Stopped;
            }
            match start_the_child(launch) {
                Ok(child) => {
                    let process_id = child.id();
                    *running = Some(process_id);
                    drop(running);
                    *last_started_process_id = Some(process_id);
                    write_the_pidfile(launch, process_id);
                    tracing::info!(
                        "{} {process_id} is running from {}",
                        launch.child_name_for_the_log,
                        launch.executable.display()
                    );
                    report(SupervisedChildProcessEvent::Started { process_id });
                    let started = Instant::now();
                    if wait_until_the_child_is_ready_or_exits(process_id, readiness_probe, stopping)
                    {
                        report(SupervisedChildProcessEvent::Ready);
                    }
                    let exit = reap_the_child_once_it_exits(child, running_child_process_id);
                    if stopping.load(Ordering::SeqCst) {
                        return SupervisedChildProcessEvent::Stopped;
                    }
                    let why_the_child_is_not_running = match exit {
                        Ok(status) => format!("it exited with {status}"),
                        Err(failure) => format!("it could not be waited on: {failure}"),
                    };
                    (why_the_child_is_not_running, started.elapsed())
                }
                Err(failure) => (
                    format!(
                        "{} could not be started: {failure}",
                        launch.executable.display()
                    ),
                    Duration::ZERO,
                ),
            }
        };
        let starting_again_after = restart_backoff
            .wait_before_starting_again_after_a_death_ending_a_run_of(run_before_the_death);
        if launch.gives_up_at_the_crash_loop_limit
            && restart_backoff.has_reached_the_crash_loop_limit_of_a_child_running_user_code()
        {
            let why = format!(
                "{} died {} times in a row soon after starting, most recently because \
                 {why_the_child_is_not_running}; it is not started again",
                launch.child_name_for_the_log,
                restart_backoff.deaths_in_a_row_before_a_healthy_run()
            );
            tracing::error!("{why}");
            return SupervisedChildProcessEvent::GivenUp { why };
        }
        tracing::warn!(
            "{} is not running, because {why_the_child_is_not_running}; it starts again in \
             {starting_again_after:?}",
            launch.child_name_for_the_log
        );
        report(SupervisedChildProcessEvent::NotRunning {
            why: why_the_child_is_not_running,
            starting_again_after,
        });
        match woken.recv_timeout(starting_again_after) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return SupervisedChildProcessEvent::Stopped;
            }
        }
    }
}

fn start_the_child(launch: &SupervisedChildProcessLaunch) -> std::io::Result<Child> {
    let mut command = Command::new(&launch.executable);
    command
        .args(&launch.arguments)
        .env_clear()
        .envs(launch.environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    let mut child = command.spawn()?;
    if let Some(standard_output) = child.stdout.take() {
        forward_the_childs_log_lines(&launch.child_name_for_the_log, standard_output);
    }
    if let Some(standard_error) = child.stderr.take() {
        forward_the_childs_log_lines(&launch.child_name_for_the_log, standard_error);
    }
    Ok(child)
}

/// Ask `readiness_probe` until it says ready, saying so, or until the child
/// exits or the supervisor is stopping, saying not.
fn wait_until_the_child_is_ready_or_exits(
    process_id: u32,
    readiness_probe: &SupervisedChildProcessReadinessProbe,
    stopping: &AtomicBool,
) -> bool {
    loop {
        if readiness_probe() {
            return true;
        }
        if stopping.load(Ordering::SeqCst)
            || a_child_process_has_exited_without_being_reaped(process_id)
        {
            return false;
        }
        std::thread::sleep(CHILD_PROCESS_POLL_INTERVAL);
    }
}

/// Wait for `child` to exit without reaping it, then clear the running slot
/// and reap it under the slot's lock, so `stop` never signals a freed pid.
fn reap_the_child_once_it_exits(
    mut child: Child,
    running_child_process_id: &Mutex<Option<u32>>,
) -> std::io::Result<ExitStatus> {
    while !a_child_process_has_exited_without_being_reaped(child.id()) {
        std::thread::sleep(CHILD_EXIT_POLL_INTERVAL);
    }
    let mut running = lock_ignoring_poison(running_child_process_id);
    *running = None;
    child.wait()
}

/// Forward each line the child writes into this process's log: its warnings
/// and errors at their own level, the rest at debug.
fn forward_the_childs_log_lines(child_name_for_the_log: &str, stream: impl Read + Send + 'static) {
    let forwarded_child_name = child_name_for_the_log.to_string();
    let forwarded = std::thread::Builder::new()
        .name(format!(
            "{}-log",
            supervisor_thread_name(child_name_for_the_log)
                .chars()
                .take(11)
                .collect::<String>()
        ))
        .spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                match supervised_child_log_line_level(&line) {
                    SupervisedChildLogLineLevel::Error => {
                        tracing::error!(child = %forwarded_child_name, "{line}")
                    }
                    SupervisedChildLogLineLevel::Warning => {
                        tracing::warn!(child = %forwarded_child_name, "{line}")
                    }
                    SupervisedChildLogLineLevel::Other => {
                        tracing::debug!(child = %forwarded_child_name, "{line}")
                    }
                }
            }
        });
    if let Err(failure) = forwarded {
        tracing::warn!("{child_name_for_the_log}'s log is not forwarded: {failure}");
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SupervisedChildLogLineLevel {
    Error,
    Warning,
    Other,
}

/// The level `tracing-subscriber`'s plain format writes after a line's
/// timestamp.
fn supervised_child_log_line_level(line: &str) -> SupervisedChildLogLineLevel {
    let mut words = line.split_whitespace();
    let _timestamp = words.next();
    match words.next() {
        Some("ERROR") => SupervisedChildLogLineLevel::Error,
        Some("WARN") => SupervisedChildLogLineLevel::Warning,
        _ => SupervisedChildLogLineLevel::Other,
    }
}

fn write_the_pidfile(launch: &SupervisedChildProcessLaunch, process_id: u32) {
    if let Err(failure) = std::fs::write(&launch.pidfile_path, format!("{process_id}\n")) {
        tracing::warn!(
            "{} {process_id}'s pidfile {} could not be written, so a crash of this runtime may \
             leave it running: {failure}",
            launch.child_name_for_the_log,
            launch.pidfile_path.display()
        );
    }
}

/// Remove `pidfile_path` only while it still holds `process_id`, so a
/// successor supervisor's pidfile for the same service is kept.
fn remove_the_pidfile_while_it_names(pidfile_path: &Path, process_id: u32) {
    let names_the_process = std::fs::read_to_string(pidfile_path)
        .is_ok_and(|pidfile| pidfile.trim().parse::<u32>() == Ok(process_id));
    if names_the_process {
        let _ = std::fs::remove_file(pidfile_path);
    }
}

/// End the child whose process id `pidfile_path` holds when it is still
/// running `executable`, and remove the pidfile.
fn end_the_child_a_previous_run_left_behind(pidfile_path: &Path, executable: &Path) {
    let Ok(pidfile) = std::fs::read_to_string(pidfile_path) else {
        return;
    };
    let _ = std::fs::remove_file(pidfile_path);
    let Ok(process_id) = pidfile.trim().parse::<u32>() else {
        return;
    };
    if !the_process_runs_the_executable(process_id, executable) {
        return;
    }
    tracing::warn!(
        "a child {process_id} of {} a previous run of this runtime left behind is ended",
        executable.display()
    );
    signal_the_process(process_id, libc::SIGKILL);
    let disappearance_deadline = Instant::now() + LEFT_BEHIND_CHILD_DISAPPEARANCE_BUDGET;
    while the_process_exists(process_id) && Instant::now() < disappearance_deadline {
        std::thread::sleep(CHILD_PROCESS_POLL_INTERVAL);
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn the_executable_of_the_process(_process_id: u32) -> Option<PathBuf> {
    None
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

fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
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
                .trim(),
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
            SupervisedChildProcess::start(launch.clone(), Box::new(|| true), Box::new(|_| {}))
                .unwrap();
        assert!(wait_until(Duration::from_secs(10), || {
            supervisor.running_child_process_id().is_some() && launch.pidfile_path.is_file()
        }));
        let successor_process_id = supervisor.running_child_process_id().unwrap() + 1;
        std::fs::write(&launch.pidfile_path, format!("{successor_process_id}\n")).unwrap();

        supervisor.stop();

        assert_eq!(
            std::fs::read_to_string(&launch.pidfile_path)
                .unwrap()
                .trim(),
            successor_process_id.to_string()
        );
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
            SupervisedChildProcess::start(launch, Box::new(|| false), events_into(&events))
                .unwrap();
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
        std::fs::write(&launch.pidfile_path, format!("{left_behind_process_id}\n")).unwrap();
        assert!(wait_until(Duration::from_secs(10), || {
            the_process_runs_the_executable(left_behind_process_id, &executable)
        }));
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
    fn a_child_log_line_keeps_its_warning_and_error_levels() {
        assert_eq!(
            supervised_child_log_line_level(
                "2026-10-18T02:21:59.085276Z  WARN moq_sock::bind: UDP receive buffer is smaller"
            ),
            SupervisedChildLogLineLevel::Warning
        );
        assert_eq!(
            supervised_child_log_line_level(
                "2026-10-18T02:21:59.085276Z ERROR moq_relay: bind failed"
            ),
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
}
