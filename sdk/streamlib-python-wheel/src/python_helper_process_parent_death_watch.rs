// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A macOS helper process's bond to its parent's life.
//!
//! Linux binds a helper to its parent with `PR_SET_PDEATHSIG` in the spawn
//! host's `pre_exec`; Darwin has no such signal, so the helper watches for
//! itself. Two signals, either one enough: kqueue `EVFILT_PROC` with
//! `NOTE_EXIT` on the parent's pid, and the dead-name notification on the
//! engine's surface-share Mach service. Whichever fires first ends the channel
//! to the parent, so the helper runs the `stop` and `teardown()` the engine can
//! no longer ask for, and walks the engine's own ladder budgets down to killing
//! its process group — nothing it waits on needs the GIL, so a processor that
//! never returns cannot hold it past the bound.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::helper_process_shutdown_ladder::{
    CALLBACK_RETURN_BUDGET, CHILD_SELF_EXIT_GRACE, TEARDOWN_BUDGET,
};

/// How long the boot-time connection to the surface-share service waits to be
/// admitted.
const SURFACE_SHARE_SERVICE_WATCH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The channel to the parent, set once the watch is armed.
static PARENT_CHANNEL_FD_THE_WATCH_ENDS: OnceLock<RawFd> = OnceLock::new();

/// Set by the first signal that fires; every later one finds the teardown
/// already under way.
static THE_PARENT_WENT_AWAY: AtomicBool = AtomicBool::new(false);

/// Set once the helper's `stop` rung has returned after its parent went away,
/// which spares the callback interrupt.
static CALLBACKS_RETURNED_AFTER_THE_PARENT_WENT_AWAY: AtomicBool = AtomicBool::new(false);

/// Arm both watches on this helper's parent, before any processor code runs.
///
/// `parent_channel_fd` is the escalate socket, shut down when a watch fires so
/// the helper reads the end of its channel. Refuses a second call, and refuses
/// when the pid watch cannot be armed — a helper that cannot watch its parent
/// could outlive it.
#[pyfunction]
pub(crate) fn watch_for_this_helper_processes_parent_going_away(
    parent_channel_fd: i32,
) -> PyResult<()> {
    if PARENT_CHANNEL_FD_THE_WATCH_ENDS
        .set(parent_channel_fd)
        .is_err()
    {
        return Err(PyRuntimeError::new_err(
            "this helper process is already watching its parent",
        ));
    }
    // SAFETY: a scalar syscall.
    let parent_process_id = unsafe { libc::getppid() };
    arm_the_parent_process_exit_watch(parent_process_id).map_err(|arm_failure| {
        PyRuntimeError::new_err(format!(
            "could not watch parent process {parent_process_id} for its exit: {arm_failure}"
        ))
    })?;
    arm_the_surface_share_service_watch();
    Ok(())
}

/// Say the helper's `stop` rung has returned, so the callback interrupt is not
/// needed.
#[pyfunction]
pub(crate) fn note_this_helper_processes_callbacks_returned_after_its_parent_went_away() {
    CALLBACKS_RETURNED_AFTER_THE_PARENT_WENT_AWAY.store(true, Ordering::SeqCst);
}

fn arm_the_parent_process_exit_watch(parent_process_id: libc::pid_t) -> io::Result<()> {
    let armed_watch = match arm_a_watch_for_the_exit_of_the_parent(parent_process_id)? {
        ProcessExitWatchArmed::Armed(kqueue_fd) => kqueue_fd,
        ProcessExitWatchArmed::ProcessAlreadyGone => {
            tear_this_helper_down_because_its_parent_went_away(
                "its parent process exited before the watch was armed",
            );
            return Ok(());
        }
    };
    std::thread::Builder::new()
        .name("parent-process-exit-watch".into())
        .spawn(move || watch_the_parent_process_until_it_exits(parent_process_id, armed_watch))?;
    Ok(())
}

/// Arm on `parent_process_id`, reading a parent that is already gone — the
/// helper reparented to launchd — as gone rather than watching launchd.
fn arm_a_watch_for_the_exit_of_the_parent(
    parent_process_id: libc::pid_t,
) -> io::Result<ProcessExitWatchArmed> {
    let armed = arm_a_watch_for_the_exit_of(parent_process_id)?;
    // SAFETY: a scalar syscall.
    if unsafe { libc::getppid() } != parent_process_id {
        return Ok(ProcessExitWatchArmed::ProcessAlreadyGone);
    }
    Ok(armed)
}

fn watch_the_parent_process_until_it_exits(
    parent_process_id: libc::pid_t,
    mut armed_watch: OwnedFd,
) {
    loop {
        match wait_for_the_watched_process_to_exit(armed_watch.as_raw_fd(), parent_process_id) {
            Ok(()) => {
                return tear_this_helper_down_because_its_parent_went_away(
                    "its parent process exited",
                );
            }
            Err(ProcessExitWatchLost::DescriptorNoLongerOurs) => {
                // A processor that closed every descriptor it could reach took
                // this one; the number may already name something else, so it
                // is let go without being closed.
                let _ = armed_watch.into_raw_fd();
                match arm_a_watch_for_the_exit_of_the_parent(parent_process_id) {
                    Ok(ProcessExitWatchArmed::Armed(rearmed_watch)) => armed_watch = rearmed_watch,
                    Ok(ProcessExitWatchArmed::ProcessAlreadyGone) => {
                        return tear_this_helper_down_because_its_parent_went_away(
                            "its parent process exited while its watch was re-armed",
                        );
                    }
                    Err(rearm_failure) => {
                        tracing::error!(
                            "could not re-arm the watch on parent process {parent_process_id} \
                             ({rearm_failure}); the surface-share service watch and the end of \
                             the parent channel remain"
                        );
                        return;
                    }
                }
            }
            Err(ProcessExitWatchLost::WaitFailed(wait_failure)) => {
                tracing::error!(
                    "the watch on parent process {parent_process_id} failed ({wait_failure}); \
                     the surface-share service watch and the end of the parent channel remain"
                );
                return;
            }
        }
    }
}

/// The belt to the pid watch's braces. A helper whose connection is refused
/// keeps the pid watch, so a refusal is logged rather than fatal.
fn arm_the_surface_share_service_watch() {
    let Ok(service_name) =
        std::env::var(streamlib_surface_client::SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE)
    else {
        return;
    };
    let connection = match streamlib_surface_client::SurfaceShareMachServiceConnection::connect(
        &service_name,
        SURFACE_SHARE_SERVICE_WATCH_HANDSHAKE_TIMEOUT,
    ) {
        Ok(connection) => connection,
        Err(connect_failure) => {
            tracing::warn!(
                "could not connect to the surface-share service '{service_name}' to watch it \
                 ({connect_failure}); this helper watches its parent's pid alone"
            );
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("surface-share-service-exit-watch".into())
        .spawn(
            move || match connection.wait_for_the_service_to_go_away(None) {
                Ok(true) => tear_this_helper_down_because_its_parent_went_away(
                    "the engine's surface-share service went away",
                ),
                Ok(false) => {}
                Err(watch_failure) => tracing::warn!(
                    "could not watch the surface-share service for its going away \
                     ({watch_failure}); this helper watches its parent's pid alone"
                ),
            },
        );
    if let Err(spawn_failure) = spawned {
        tracing::warn!(
            "could not start the surface-share service watch ({spawn_failure}); this helper \
             watches its parent's pid alone"
        );
    }
}

/// Start the self-teardown once, whichever signal asks first.
fn tear_this_helper_down_because_its_parent_went_away(reason: &'static str) {
    if THE_PARENT_WENT_AWAY.swap(true, Ordering::SeqCst) {
        return;
    }
    tracing::warn!("{reason}; this helper tears itself down");
    let the_parent_went_away_at = Instant::now();
    if let Some(parent_channel_fd) = PARENT_CHANNEL_FD_THE_WATCH_ENDS.get() {
        // SAFETY: a scalar syscall on the channel `_helper` handed over. Shut
        // down rather than closed: the bridge's reader still owns the fd, and
        // shutting it down is what wakes that reader with the end of the
        // channel even while something else still holds the parent's end.
        unsafe { libc::shutdown(*parent_channel_fd, libc::SHUT_RDWR) };
    }
    let walked = std::thread::Builder::new()
        .name("parent-went-away-teardown".into())
        .spawn(move || walk_the_self_teardown_ladder_from(the_parent_went_away_at));
    if walked.is_err() {
        // Without the thread nothing bounds the teardown, so the bound is
        // taken now.
        kill_this_helpers_whole_process_group();
    }
}

fn walk_the_self_teardown_ladder_from(the_parent_went_away_at: Instant) -> ! {
    let mut callback_already_interrupted = false;
    loop {
        match next_self_teardown_step(
            the_parent_went_away_at.elapsed(),
            CALLBACKS_RETURNED_AFTER_THE_PARENT_WENT_AWAY.load(Ordering::SeqCst),
            callback_already_interrupted,
        ) {
            SelfTeardownStep::WaitAtMost(remaining) => std::thread::sleep(remaining),
            SelfTeardownStep::InterruptTheCallbackStillRunning => {
                callback_already_interrupted = true;
                tracing::warn!(
                    "this helper was still in a callback {}s after its parent went away; \
                     interrupting it",
                    CALLBACK_RETURN_BUDGET.as_secs()
                );
                // SAFETY: a scalar syscall to this process. A real signal, as
                // the engine's ladder sends: only a signal wakes a main thread
                // asleep inside a blocking call.
                unsafe { libc::kill(libc::getpid(), libc::SIGINT) };
            }
            SelfTeardownStep::KillTheWholeProcessGroup => kill_this_helpers_whole_process_group(),
        }
    }
}

/// End the helper and everything in its group, as the engine's ladder ends
/// every helper.
fn kill_this_helpers_whole_process_group() -> ! {
    // SAFETY: scalar syscalls. The spawn host made this helper the leader of a
    // group whose id is its pid; the pid itself is signalled too, for a
    // processor that moved the helper out of that group.
    unsafe {
        let this_helper = libc::getpid();
        libc::killpg(this_helper, libc::SIGKILL);
        libc::kill(this_helper, libc::SIGKILL);
        libc::_exit(1)
    }
}

/// What the self-teardown does next, `since_the_parent_went_away`.
#[derive(Debug, PartialEq, Eq)]
enum SelfTeardownStep {
    WaitAtMost(Duration),
    InterruptTheCallbackStillRunning,
    KillTheWholeProcessGroup,
}

/// The engine's ladder, walked by the helper itself: a callback that has not
/// returned within its budget is interrupted, and whatever is still alive once
/// `teardown()` and the helper's own exit have had theirs is killed.
fn next_self_teardown_step(
    since_the_parent_went_away: Duration,
    callbacks_returned: bool,
    callback_already_interrupted: bool,
) -> SelfTeardownStep {
    let kill_at = self_teardown_bound();
    if since_the_parent_went_away >= kill_at {
        return SelfTeardownStep::KillTheWholeProcessGroup;
    }
    if !callbacks_returned && !callback_already_interrupted {
        if since_the_parent_went_away >= CALLBACK_RETURN_BUDGET {
            return SelfTeardownStep::InterruptTheCallbackStillRunning;
        }
        return SelfTeardownStep::WaitAtMost(CALLBACK_RETURN_BUDGET - since_the_parent_went_away);
    }
    SelfTeardownStep::WaitAtMost(kill_at - since_the_parent_went_away)
}

/// How long after its parent goes away a helper can still be alive.
fn self_teardown_bound() -> Duration {
    CALLBACK_RETURN_BUDGET + TEARDOWN_BUDGET + CHILD_SELF_EXIT_GRACE
}

/// What arming a watch on a process's exit found.
#[derive(Debug)]
enum ProcessExitWatchArmed {
    Armed(OwnedFd),
    ProcessAlreadyGone,
}

/// Why a watch on a process's exit stopped without seeing it.
#[derive(Debug)]
enum ProcessExitWatchLost {
    /// The kqueue was closed under the watch, or its number now names
    /// something else.
    DescriptorNoLongerOurs,
    WaitFailed(io::Error),
}

/// A kqueue holding one `EVFILT_PROC` / `NOTE_EXIT` registration on
/// `process_id`.
fn arm_a_watch_for_the_exit_of(process_id: libc::pid_t) -> io::Result<ProcessExitWatchArmed> {
    // SAFETY: kqueue returns -1 on failure; checked below.
    let raw_kqueue_fd = unsafe { libc::kqueue() };
    if raw_kqueue_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: raw_kqueue_fd was just opened here and nothing else owns it.
    let kqueue_fd = unsafe { OwnedFd::from_raw_fd(raw_kqueue_fd) };
    // Darwin has no `kqueue1`, so close-on-exec is set before the fd is used.
    // SAFETY: a scalar syscall on an fd this function owns.
    if unsafe { libc::fcntl(kqueue_fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let registration = libc::kevent {
        ident: process_id as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one fully initialized change, no event buffer.
    let registered = unsafe {
        libc::kevent(
            kqueue_fd.as_raw_fd(),
            &registration,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if registered < 0 {
        let registration_failure = io::Error::last_os_error();
        if registration_failure.raw_os_error() == Some(libc::ESRCH) {
            return Ok(ProcessExitWatchArmed::ProcessAlreadyGone);
        }
        return Err(registration_failure);
    }
    Ok(ProcessExitWatchArmed::Armed(kqueue_fd))
}

/// Block until the process `kqueue_fd` watches exits.
fn wait_for_the_watched_process_to_exit(
    kqueue_fd: RawFd,
    process_id: libc::pid_t,
) -> Result<(), ProcessExitWatchLost> {
    loop {
        // SAFETY: an all-zero `kevent` is a valid out-slot.
        let mut delivered: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: no changes, one out-slot this frame owns, no timeout.
        let delivered_count = unsafe {
            libc::kevent(
                kqueue_fd,
                std::ptr::null(),
                0,
                &mut delivered,
                1,
                std::ptr::null(),
            )
        };
        if delivered_count < 0 {
            let wait_failure = io::Error::last_os_error();
            return match wait_failure.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EBADF) | Some(libc::EINVAL) => {
                    Err(ProcessExitWatchLost::DescriptorNoLongerOurs)
                }
                _ => Err(ProcessExitWatchLost::WaitFailed(wait_failure)),
            };
        }
        if delivered_count == 0 {
            continue;
        }
        let is_this_watch = delivered.filter == libc::EVFILT_PROC
            && delivered.ident == process_id as libc::uintptr_t;
        if !is_this_watch {
            return Err(ProcessExitWatchLost::DescriptorNoLongerOurs);
        }
        if delivered.fflags & libc::NOTE_EXIT != 0 {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    fn a_process_parked_until_killed() -> Child {
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn /bin/sleep")
    }

    #[test]
    fn a_callback_that_has_not_returned_is_given_its_budget_first() {
        assert_eq!(
            next_self_teardown_step(Duration::from_millis(200), false, false),
            SelfTeardownStep::WaitAtMost(CALLBACK_RETURN_BUDGET - Duration::from_millis(200))
        );
    }

    #[test]
    fn a_callback_that_outran_its_budget_is_interrupted_once() {
        assert_eq!(
            next_self_teardown_step(CALLBACK_RETURN_BUDGET, false, false),
            SelfTeardownStep::InterruptTheCallbackStillRunning
        );
        assert_eq!(
            next_self_teardown_step(CALLBACK_RETURN_BUDGET, false, true),
            SelfTeardownStep::WaitAtMost(self_teardown_bound() - CALLBACK_RETURN_BUDGET)
        );
    }

    #[test]
    fn callbacks_that_returned_are_never_interrupted() {
        assert_eq!(
            next_self_teardown_step(CALLBACK_RETURN_BUDGET * 2, true, false),
            SelfTeardownStep::WaitAtMost(self_teardown_bound() - CALLBACK_RETURN_BUDGET * 2)
        );
    }

    #[test]
    fn whatever_is_alive_at_the_bound_is_killed() {
        for (callbacks_returned, interrupted) in [(false, false), (false, true), (true, false)] {
            assert_eq!(
                next_self_teardown_step(self_teardown_bound(), callbacks_returned, interrupted),
                SelfTeardownStep::KillTheWholeProcessGroup
            );
        }
    }

    #[test]
    fn a_process_already_gone_is_reported_rather_than_watched() {
        let mut gone = a_process_parked_until_killed();
        let gone_process_id = gone.id() as libc::pid_t;
        gone.kill().expect("kill the parked process");
        gone.wait().expect("reap the parked process");

        assert!(matches!(
            arm_a_watch_for_the_exit_of(gone_process_id).expect("arm"),
            ProcessExitWatchArmed::ProcessAlreadyGone
        ));
    }

    /// The one wiring check of the kqueue arm: nothing else proves the
    /// registration the watch blocks on is the one a process's exit fires.
    #[test]
    fn the_watch_fires_when_the_watched_process_exits() {
        let mut watched = a_process_parked_until_killed();
        let watched_process_id = watched.id() as libc::pid_t;
        let ProcessExitWatchArmed::Armed(kqueue_fd) =
            arm_a_watch_for_the_exit_of(watched_process_id).expect("arm")
        else {
            panic!("the parked process is alive");
        };
        let (fired_sender, fired_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let waited =
                wait_for_the_watched_process_to_exit(kqueue_fd.as_raw_fd(), watched_process_id);
            let _ = fired_sender.send(waited.is_ok());
        });

        watched.kill().expect("kill the watched process");
        watched.wait().expect("reap the watched process");

        assert_eq!(
            fired_receiver.recv_timeout(Duration::from_secs(10)),
            Ok(true),
            "the watch never saw the process exit"
        );
    }
}
