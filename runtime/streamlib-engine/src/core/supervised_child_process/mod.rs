// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A long-lived native child the runtime keeps running — a shared native
//! service — from its start until it is stopped.
//!
//! The child gets the preparation every child of the runtime gets — its own
//! process group, a lifetime bound to the runtime's where the platform can, no
//! descriptor beyond its standard streams — and an empty environment but for
//! the variables its launch names. Its process id is written to a pidfile, and
//! a child a crashed runtime left behind is found by that file and ended
//! before a new one starts. A supervisor holds an exclusive lock beside the
//! pidfile while it runs, so a second supervisor of the same child is refused
//! rather than ending a child a live one keeps. A child that exits is started again under the one
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

/// The most of one child log line forwarded; the rest of a longer line is
/// dropped.
const MOST_FORWARDED_CHILD_LOG_LINE_BYTES: usize = 16 * 1024;

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
    _supervisor_lock_held_until_dropped: std::fs::File,
}

impl SupervisedChildProcess {
    /// End a child a previous run left behind, then start the child and keep
    /// it running, reporting each change to `report`.
    pub fn start(
        launch: SupervisedChildProcessLaunch,
        readiness_probe: SupervisedChildProcessReadinessProbe,
        report: SupervisedChildProcessEventSink,
    ) -> std::io::Result<Self> {
        let supervisor_lock = take_the_supervisor_lock_of(&launch)?;
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
            _supervisor_lock_held_until_dropped: supervisor_lock,
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
            let mut child_output = BufReader::new(stream);
            let mut line_bytes = Vec::new();
            while let Ok(true) = read_one_line_keeping_at_most(
                &mut child_output,
                &mut line_bytes,
                MOST_FORWARDED_CHILD_LOG_LINE_BYTES,
            ) {
                let line = String::from_utf8_lossy(&line_bytes);
                let line = line.trim_end_matches(['\n', '\r']);
                match supervised_child_log_line_level(line) {
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

/// Read through the next newline into `line_bytes`, keeping at most
/// `most_bytes_kept` of it and dropping the rest; `false` at the end of the
/// output.
fn read_one_line_keeping_at_most(
    child_output: &mut impl BufRead,
    line_bytes: &mut Vec<u8>,
    most_bytes_kept: usize,
) -> std::io::Result<bool> {
    line_bytes.clear();
    let mut read_anything = false;
    loop {
        let available = child_output.fill_buf()?;
        if available.is_empty() {
            return Ok(read_anything);
        }
        read_anything = true;
        let newline_at = available.iter().position(|byte| *byte == b'\n');
        let through = newline_at.map_or(available.len(), |newline_at| newline_at + 1);
        let room = most_bytes_kept.saturating_sub(line_bytes.len());
        line_bytes.extend_from_slice(&available[..through.min(room)]);
        child_output.consume(through);
        if newline_at.is_some() {
            return Ok(true);
        }
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

/// A pidfile's lines: the child's process id, then the start time the system
/// gave that process, so a later run ends it only while that process id still
/// names the same process.
fn the_pidfile_text_of(process_id: u32) -> String {
    match the_start_time_of_the_process(process_id) {
        Some(start_time) => format!("{process_id}\n{start_time}\n"),
        None => format!("{process_id}\n"),
    }
}

/// The process id and start time a pidfile's text names.
fn the_process_a_pidfile_names(pidfile_text: &str) -> Option<(u32, Option<String>)> {
    let mut lines = pidfile_text.lines();
    let process_id = lines.next()?.trim().parse::<u32>().ok()?;
    let start_time = lines
        .next()
        .map(str::trim)
        .filter(|start_time| !start_time.is_empty())
        .map(str::to_string);
    Some((process_id, start_time))
}

fn write_the_pidfile(launch: &SupervisedChildProcessLaunch, process_id: u32) {
    if let Err(failure) = std::fs::write(&launch.pidfile_path, the_pidfile_text_of(process_id)) {
        tracing::warn!(
            "{} {process_id}'s pidfile {} could not be written, so a crash of this runtime may \
             leave it running: {failure}",
            launch.child_name_for_the_log,
            launch.pidfile_path.display()
        );
    }
}

/// The lock file beside the launch's pidfile, locked exclusively; refused
/// while another live supervisor holds it.
fn take_the_supervisor_lock_of(
    launch: &SupervisedChildProcessLaunch,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let lock_path = launch.pidfile_path.with_extension("lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    // SAFETY: flock on a descriptor this function owns.
    if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let failure = std::io::Error::last_os_error();
        if failure.kind() == std::io::ErrorKind::WouldBlock {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "another live supervisor holds {}, so {} is already kept running",
                    lock_path.display(),
                    launch.child_name_for_the_log
                ),
            ));
        }
        return Err(failure);
    }
    Ok(lock_file)
}

/// Remove `pidfile_path` only while it still holds `process_id`, so a
/// successor supervisor's pidfile for the same service is kept.
fn remove_the_pidfile_while_it_names(pidfile_path: &Path, process_id: u32) {
    let names_the_process = std::fs::read_to_string(pidfile_path).is_ok_and(|pidfile| {
        the_process_a_pidfile_names(&pidfile)
            .is_some_and(|(named_process_id, _)| named_process_id == process_id)
    });
    if names_the_process {
        let _ = std::fs::remove_file(pidfile_path);
    }
}

/// End the child `pidfile_path` names when that process id still names the
/// process the pidfile recorded, started at the same time and running
/// `executable`, and remove the pidfile.
fn end_the_child_a_previous_run_left_behind(pidfile_path: &Path, executable: &Path) {
    let Ok(pidfile) = std::fs::read_to_string(pidfile_path) else {
        return;
    };
    let _ = std::fs::remove_file(pidfile_path);
    let Some((process_id, Some(recorded_start_time))) = the_process_a_pidfile_names(&pidfile)
    else {
        return;
    };
    if the_start_time_of_the_process(process_id).as_deref() != Some(recorded_start_time.as_str())
        || !the_process_runs_the_executable(process_id, executable)
    {
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

/// When the system started process `process_id`, in its own clock's units: a
/// reused process id names a process with another start time.
#[cfg(target_os = "linux")]
fn the_start_time_of_the_process(process_id: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{process_id}/stat")).ok()?;
    // `/proc/<pid>/stat`'s 22nd field; the fields after the parenthesised
    // command name start at the 3rd.
    let after_the_command_name = &stat[stat.rfind(')')? + 1..];
    after_the_command_name
        .split_whitespace()
        .nth(19)
        .map(str::to_string)
}

#[cfg(target_os = "macos")]
fn the_start_time_of_the_process(process_id: u32) -> Option<String> {
    let process_id = libc::c_int::try_from(process_id).ok()?;
    let mut process_information = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let information_size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: the buffer is one `proc_bsdinfo` and that is the size passed;
    // `proc_pidinfo` writes at most that many bytes and returns how many.
    let written = unsafe {
        libc::proc_pidinfo(
            process_id,
            libc::PROC_PIDTBSDINFO,
            0,
            process_information.as_mut_ptr().cast(),
            libc::c_int::try_from(information_size).ok()?,
        )
    };
    if usize::try_from(written).ok()? != information_size {
        return None;
    }
    // SAFETY: `proc_pidinfo` filled the whole struct, which was zeroed first.
    let process_information = unsafe { process_information.assume_init() };
    Some(format!(
        "{}.{:06}",
        process_information.pbi_start_tvsec, process_information.pbi_start_tvusec
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn the_start_time_of_the_process(_process_id: u32) -> Option<String> {
    None
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
mod tests;
