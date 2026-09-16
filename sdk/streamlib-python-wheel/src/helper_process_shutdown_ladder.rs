// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The bounded ladder one helper process is stopped on.
//!
//! `docs/plan/ARCHITECTURE.md` §Processor model, the `[shutdown-ladder]` entry:
//! shutdown always ends, and a cooperative processor's `teardown()` always
//! runs. The rungs are `stop` and `teardown` sent together, a one-second
//! interrupt for a callback that has not returned, five seconds for
//! `teardown()`, the child's own window to leave, then its whole process group
//! terminated, killed, and reaped — or the child abandoned and named.
//!
//! The ladder owns the child rather than borrowing it, and nothing here reaps
//! it until the last signal has gone out: reaping a group leader frees its pid,
//! and the process group id equals that pid, so a group signalled after the
//! reap could land on whoever the OS handed it to next. Every wait is bounded.

use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

use streamlib::sdk::helper_process_transport::HelperProcessShutdownCommand;

/// How long a Python callback has to return before the ladder interrupts it.
///
/// Engine-chosen and not authorable: the plan makes every budget here the
/// engine's, so none is reachable from a processor's configuration.
const CALLBACK_RETURN_BUDGET: Duration = Duration::from_secs(1);

/// How long `teardown()` has once the helper has been asked for it.
const TEARDOWN_BUDGET: Duration = Duration::from_secs(5);

/// How long the child has to leave on its own once its hooks have returned.
///
/// Spent before any signal: a helper that answered `done` is already on its way
/// out, and terminating it mid-finalization would cut short the teardown the
/// ladder just waited for — and leave the iceoryx2 node its engine half holds
/// registered as a dead one.
const CHILD_SELF_EXIT_GRACE: Duration = Duration::from_millis(500);

/// How long the helper's process group has to leave on `SIGTERM` before it is
/// killed.
const PROCESS_GROUP_TERMINATION_GRACE: Duration = Duration::from_millis(500);

/// How long the reap waits for a killed child to become collectable.
const REAP_BUDGET: Duration = Duration::from_secs(1);

/// How often a bounded wait for the child's exit re-checks.
const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How one helper's shutdown ended.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub(crate) enum HelperProcessShutdownOutcome {
    /// The child exited and was reaped, leaving no survivor.
    Reaped(ExitStatus),
    /// The child outlived every rung. It is named rather than waited on.
    AbandonedAfterTheLadder,
}

/// Whether `process_id` has exited, leaving it collectable but not collected.
///
/// `WNOWAIT` is the whole point: the zombie stays, so the pid — and the process
/// group id that equals it — cannot be handed to anybody else between this
/// answer and the signal that follows it. `Child::try_wait` cannot be used for
/// the same question, because it collects.
pub(crate) fn a_helper_process_has_exited_without_being_reaped(process_id: u32) -> bool {
    // SAFETY: a zeroed `siginfo_t` is a valid buffer for `waitid` to fill, and
    // the call reports rather than collects.
    let mut reported: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `reported` is a valid, live `siginfo_t` for the duration.
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            process_id as libc::id_t,
            &mut reported,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    // `WNOHANG` returns 0 with nothing reported while the child is still
    // running, and the zeroed pid is how that case is told from a real one.
    waited == 0 && reported_process_id(&reported) == process_id as libc::pid_t
}

#[cfg(target_os = "linux")]
fn reported_process_id(reported: &libc::siginfo_t) -> libc::pid_t {
    // SAFETY: `si_pid` reads the union arm a `SIGCHLD`-shaped report fills.
    unsafe { reported.si_pid() }
}

#[cfg(not(target_os = "linux"))]
fn reported_process_id(reported: &libc::siginfo_t) -> libc::pid_t {
    reported.si_pid
}

pub(crate) struct HelperProcessShutdownLadder {
    processor_display_name: String,
    child: Child,
}

impl HelperProcessShutdownLadder {
    pub(crate) fn taking_over(processor_display_name: String, child: Child) -> Self {
        Self {
            processor_display_name,
            child,
        }
    }

    /// Walk every rung, asking `await_lifecycle_reply` for each cooperative one.
    ///
    /// Both commands are already on the wire when this is called — the host
    /// sends them together — so `await_lifecycle_reply` only answers whether
    /// that command's reply arrived inside the budget it was handed, draining
    /// whatever else the helper sent on the way. A closure rather than the
    /// bridge itself, so the rungs are drivable against a stub child with no
    /// engine behind it.
    pub(crate) fn walk_every_rung(
        mut self,
        mut await_lifecycle_reply: impl FnMut(HelperProcessShutdownCommand, Duration) -> bool,
    ) -> HelperProcessShutdownOutcome {
        // A helper that is already gone answers nothing and needs no interrupt.
        // Skipped rather than waited out, so a crash before shutdown does not
        // buy six seconds and a callback warning naming a zombie.
        if !a_helper_process_has_exited_without_being_reaped(self.child.id()) {
            self.walk_the_cooperative_rungs(&mut await_lifecycle_reply);
        }
        self.end_the_process_group_and_reap()
    }

    /// Take the group down with no cooperative rung, for the paths that have
    /// none: a start the engine refused, a crash it detected, a host dropped
    /// before teardown could run.
    pub(crate) fn skip_to_terminating_the_process_group(mut self) -> HelperProcessShutdownOutcome {
        self.end_the_process_group_and_reap()
    }

    fn walk_the_cooperative_rungs(
        &mut self,
        await_lifecycle_reply: &mut impl FnMut(HelperProcessShutdownCommand, Duration) -> bool,
    ) {
        if !await_lifecycle_reply(HelperProcessShutdownCommand::Stop, CALLBACK_RETURN_BUDGET) {
            // A real signal, not `_thread.interrupt_main()`: only a signal
            // wakes a main thread asleep inside a blocking call.
            self.signal_the_child_itself(libc::SIGINT);
            tracing::warn!(
                "[{}] its helper process was still in a callback after {}s; interrupting it. \
                 The bag in flight is lost.",
                self.processor_display_name,
                CALLBACK_RETURN_BUDGET.as_secs(),
            );
        }

        if !await_lifecycle_reply(HelperProcessShutdownCommand::Teardown, TEARDOWN_BUDGET) {
            tracing::warn!(
                "[{}] its helper process did not finish teardown within {}s",
                self.processor_display_name,
                TEARDOWN_BUDGET.as_secs(),
            );
        }
    }

    /// Let the child leave, then terminate, kill and reap its whole group.
    ///
    /// Both signals go out whatever the helper itself did, because the plan
    /// puts the group down at *every* helper exit: a helper that answered
    /// `done` and left can still have forked a worker, and a signal skipped
    /// because the child is already a zombie is the survivor the rung exists to
    /// prevent. Neither reaches a descendant that left the group on purpose —
    /// the plan's stated residual.
    ///
    /// The reap is last and alone, because it is the only step that collects:
    /// a reaped leader's pid, and the group id equal to it, are free for reuse
    /// the moment it returns.
    fn end_the_process_group_and_reap(&mut self) -> HelperProcessShutdownOutcome {
        self.wait_for_the_child_to_become_collectable(CHILD_SELF_EXIT_GRACE);

        // The group, never the pid: a fork-based worker or an `os.system`
        // child survives a signal to the helper alone, and it holds the
        // helper's sockets open behind it.
        self.signal_the_whole_process_group(libc::SIGTERM);
        self.wait_for_the_child_to_become_collectable(PROCESS_GROUP_TERMINATION_GRACE);

        self.signal_the_whole_process_group(libc::SIGKILL);
        self.reap_the_child_within(REAP_BUDGET)
    }

    fn signal_the_child_itself(&self, signal: libc::c_int) {
        // SAFETY: the child is held unreaped, so its pid still names it.
        unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
    }

    /// `pre_exec` puts every helper in a group of its own whose id is its pid,
    /// and nothing has reaped the child yet, so that pid is still this group's.
    fn signal_the_whole_process_group(&self, signal: libc::c_int) {
        // SAFETY: as above — the pid is still this child's, and its own.
        unsafe { libc::killpg(self.child.id() as libc::pid_t, signal) };
    }

    /// Wait up to `budget` for the child to exit, without collecting it.
    fn wait_for_the_child_to_become_collectable(&self, budget: Duration) {
        let deadline = Instant::now() + budget;
        while !a_helper_process_has_exited_without_being_reaped(self.child.id()) {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        }
    }

    fn reap_the_child_within(&mut self, budget: Duration) -> HelperProcessShutdownOutcome {
        let deadline = Instant::now() + budget;
        loop {
            match self.child.try_wait() {
                Ok(Some(exit_status)) => return HelperProcessShutdownOutcome::Reaped(exit_status),
                Ok(None) => {}
                Err(uncollectable) => {
                    tracing::error!(
                        "[{}] its helper process (pid={}) cannot be collected: {uncollectable}",
                        self.processor_display_name,
                        self.child.id(),
                    );
                    return HelperProcessShutdownOutcome::AbandonedAfterTheLadder;
                }
            }
            if Instant::now() >= deadline {
                // Uninterruptible sleep inside a driver is the case user space
                // cannot end. Naming it beats waiting out an app that will
                // never be allowed to quit.
                tracing::error!(
                    "[{}] its helper process (pid={}) outlived the shutdown ladder and is \
                     abandoned unreaped; it is not killable from user space",
                    self.processor_display_name,
                    self.child.id(),
                );
                return HelperProcessShutdownOutcome::AbandonedAfterTheLadder;
            }
            std::thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Lines};
    use std::os::unix::process::CommandExt;
    use std::process::{ChildStdout, Command, Stdio};

    /// A stub child under `python -c`, in its own process group the way a real
    /// helper is, so the group rungs have something to signal.
    ///
    /// The source must print one line the moment it is ready — signal handlers
    /// installed, worker forked — and [`StubHelperChild::next_reported_line`]
    /// reads it. Without that handshake the ladder outruns a cold interpreter
    /// and signals a process that has not reached its own handlers yet.
    struct StubHelperChild {
        child: Child,
        reported_lines: Lines<BufReader<ChildStdout>>,
    }

    impl StubHelperChild {
        fn running(python_source: &str) -> Self {
            let mut command = Command::new("python3");
            command
                .arg("-c")
                .arg(python_source)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            // SAFETY: `setpgid` is async-signal-safe, the contract for a
            // `pre_exec` closure running between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().expect("the stub helper to start");
            let reported_lines =
                BufReader::new(child.stdout.take().expect("the stub's stdout")).lines();
            Self {
                child,
                reported_lines,
            }
        }

        fn next_reported_line(&mut self) -> String {
            self.reported_lines
                .next()
                .expect("the stub helper to report a line")
                .expect("the stub helper's line to read")
        }

        fn into_ladder(self, processor_display_name: &str) -> HelperProcessShutdownLadder {
            HelperProcessShutdownLadder::taking_over(processor_display_name.to_string(), self.child)
        }
    }

    /// A stub that installs `SIGINT` and `SIGTERM` dispositions, then reports
    /// ready and parks. `python -c` source, so indentation is spelled with the
    /// escape a shell-free literal can carry.
    fn a_stub_parking_after(dispositions: &str) -> StubHelperChild {
        StubHelperChild::running(&format!(
            r#"
import signal, sys, time
{dispositions}
sys.stdout.write("ready\n")
sys.stdout.flush()
while True:
    time.sleep(0.05)
"#
        ))
    }

    /// Dispositions for a stub no catchable signal can end, which is what
    /// makes the kill rung the one that reaches it.
    const IGNORES_EVERY_CATCHABLE_SIGNAL: &str = r#"
signal.signal(signal.SIGINT, signal.SIG_IGN)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
"#;

    /// Neither rung is ever answered, and each budget is spent in full.
    fn never_answers(_: HelperProcessShutdownCommand, budget: Duration) -> bool {
        std::thread::sleep(budget);
        false
    }

    /// Neither rung is answered and no budget is spent, which is how a test
    /// reaches the group rungs without waiting six seconds for them.
    fn never_answers_without_waiting(_: HelperProcessShutdownCommand, _: Duration) -> bool {
        false
    }

    /// Neither rung is answered, but the teardown rung spends a slice of its
    /// budget — which is what gives an interrupted callback time to unwind. The
    /// ladder's own five seconds, shortened so the test is not five seconds.
    fn never_answers_but_lets_an_interrupted_callback_unwind(
        command: HelperProcessShutdownCommand,
        _: Duration,
    ) -> bool {
        if command == HelperProcessShutdownCommand::Teardown {
            std::thread::sleep(Duration::from_millis(500));
        }
        false
    }

    fn answers_at_once(_: HelperProcessShutdownCommand, _: Duration) -> bool {
        true
    }

    /// The exit code a stub's own `SIGINT` handler leaves through, so a test
    /// can tell an interrupted helper from a terminated one by its status.
    const EXIT_CODE_OF_A_STUB_THAT_TOOK_THE_INTERRUPT: i32 = 7;

    const LEAVES_THROUGH_THE_INTERRUPT: &str = r#"
def leave_through_the_interrupt(*_):
    sys.exit(7)
signal.signal(signal.SIGINT, leave_through_the_interrupt)
"#;

    fn exit_code_of(outcome: &HelperProcessShutdownOutcome) -> Option<i32> {
        match outcome {
            HelperProcessShutdownOutcome::Reaped(exit_status) => exit_status.code(),
            HelperProcessShutdownOutcome::AbandonedAfterTheLadder => None,
        }
    }

    fn terminating_signal_of(outcome: &HelperProcessShutdownOutcome) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        match outcome {
            HelperProcessShutdownOutcome::Reaped(exit_status) => exit_status.signal(),
            HelperProcessShutdownOutcome::AbandonedAfterTheLadder => None,
        }
    }

    #[test]
    fn a_running_helper_is_not_reported_dead_and_the_same_helper_is_once_it_is() {
        // The probe's negative arm, which nothing else locks: invert it and
        // every other test here still passes while the Manual loop kills every
        // live helper a hundred milliseconds after it starts.
        let mut stub = a_stub_parking_after("");
        assert_eq!(stub.next_reported_line(), "ready");
        let process_id = stub.child.id();

        assert!(
            !a_helper_process_has_exited_without_being_reaped(process_id),
            "a helper that is still parked was reported dead"
        );

        // SAFETY: the pid is this test's own unreaped child's.
        unsafe { libc::kill(process_id as libc::pid_t, libc::SIGKILL) };
        assert!(
            a_pid_becomes_collectable_within(process_id, Duration::from_secs(5)),
            "a killed helper was never reported dead"
        );
        let _ = stub.child.wait();
    }

    fn a_pid_becomes_collectable_within(process_id: u32, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while !a_helper_process_has_exited_without_being_reaped(process_id) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        }
        true
    }

    #[test]
    fn a_helper_still_in_a_callback_is_interrupted_with_a_real_signal() {
        // `_thread.interrupt_main()` cannot wake a main thread inside
        // `time.sleep`; the stub's own handler running is what says a signal
        // that can arrived instead.
        let mut stub = a_stub_parking_after(LEAVES_THROUGH_THE_INTERRUPT);
        assert_eq!(stub.next_reported_line(), "ready");

        let ladder = stub.into_ladder("SleepyProbe");
        let outcome = ladder.walk_every_rung(never_answers_but_lets_an_interrupted_callback_unwind);

        assert_eq!(
            exit_code_of(&outcome),
            Some(EXIT_CODE_OF_A_STUB_THAT_TOOK_THE_INTERRUPT),
            "the helper left through some other door than the interrupt: {outcome:?}"
        );
    }

    #[test]
    fn a_helper_that_leaves_on_its_own_is_never_signalled_at_all() {
        // The self-exit grace, which is what keeps a cooperative shutdown from
        // ending in a SIGTERM through the middle of interpreter finalization —
        // and from leaving the iceoryx2 node that finalization drops
        // registered as a dead one.
        //
        // Fail-without-fix: take the grace out and the group signal lands
        // while this stub is still on its way out, so it is reaped with a
        // terminating signal rather than its own exit code.
        let mut stub = StubHelperChild::running(
            r#"
import sys, time
sys.stdout.write("ready\n")
sys.stdout.flush()
time.sleep(0.2)
raise SystemExit(0)
"#,
        );
        assert_eq!(stub.next_reported_line(), "ready");

        let ladder = stub.into_ladder("LeavesOnItsOwnProbe");
        let outcome = ladder.walk_every_rung(answers_at_once);

        assert_eq!(
            terminating_signal_of(&outcome),
            None,
            "a helper on its way out was signalled before it got there: {outcome:?}"
        );
        assert_eq!(exit_code_of(&outcome), Some(0));
    }

    #[test]
    fn a_helper_that_ignores_the_interrupt_and_the_termination_is_killed_and_reaped() {
        let mut stub = a_stub_parking_after(IGNORES_EVERY_CATCHABLE_SIGNAL);
        assert_eq!(stub.next_reported_line(), "ready");

        let ladder = stub.into_ladder("WedgedProbe");
        let outcome = ladder.walk_every_rung(never_answers_without_waiting);

        assert_eq!(
            terminating_signal_of(&outcome),
            Some(libc::SIGKILL),
            "a helper that ignores every catchable signal must reach the kill rung: {outcome:?}"
        );
    }

    #[test]
    fn a_cooperative_helper_is_never_interrupted_and_its_group_still_goes() {
        // Its SIGINT handler would exit 7, so the absence of that code is what
        // says no interrupt was sent; the termination is what ends it instead,
        // because the plan puts the group down at every helper exit — a helper
        // that answered `done` can still have forked a worker.
        let mut stub = a_stub_parking_after(LEAVES_THROUGH_THE_INTERRUPT);
        assert_eq!(stub.next_reported_line(), "ready");

        let ladder = stub.into_ladder("TidyProbe");
        let outcome = ladder.walk_every_rung(answers_at_once);

        assert_ne!(
            exit_code_of(&outcome),
            Some(EXIT_CODE_OF_A_STUB_THAT_TOOK_THE_INTERRUPT),
            "a helper that answered inside its budget must never be signalled"
        );
        assert_eq!(
            terminating_signal_of(&outcome),
            Some(libc::SIGTERM),
            "the group still goes down after a cooperative shutdown: {outcome:?}"
        );
    }

    /// A stub that forks a worker, reports its pid, and parks. The worker
    /// outlives its parent deliberately — it is the survivor the group rung
    /// exists to reach.
    fn a_stub_that_forked_a_worker() -> (StubHelperChild, libc::pid_t) {
        let mut stub = StubHelperChild::running(
            r#"
import os, sys, time
worker = os.fork()
if worker == 0:
    time.sleep(120)
    os._exit(0)
sys.stdout.write(str(worker) + "\n")
sys.stdout.flush()
time.sleep(120)
"#,
        );
        let worker_pid = stub
            .next_reported_line()
            .parse()
            .expect("the stub to report its worker's pid");
        (stub, worker_pid)
    }

    #[test]
    fn a_worker_the_helper_forked_dies_with_the_helpers_group() {
        let (stub, worker_pid) = a_stub_that_forked_a_worker();
        let ladder = stub.into_ladder("ForkingProbe");

        let _ = ladder.walk_every_rung(never_answers_without_waiting);

        assert!(
            a_pid_is_gone_within(worker_pid, Duration::from_secs(5)),
            "the worker survived a ladder that signalled only the helper's pid"
        );
    }

    #[test]
    fn a_crash_path_takes_the_group_down_with_no_cooperative_rung() {
        let (stub, worker_pid) = a_stub_that_forked_a_worker();
        let ladder = stub.into_ladder("CrashedProbe");

        let outcome = ladder.skip_to_terminating_the_process_group();

        assert!(matches!(outcome, HelperProcessShutdownOutcome::Reaped(_)));
        assert!(
            a_pid_is_gone_within(worker_pid, Duration::from_secs(5)),
            "a detected crash must take the helper's descendants with it"
        );
    }

    #[test]
    fn the_whole_ladder_is_bounded_well_inside_the_apps_own_watchdog() {
        // Every budget spent in full, against a child that answers nothing and
        // ignores every catchable signal — the worst case the ladder admits.
        let mut stub = a_stub_parking_after(IGNORES_EVERY_CATCHABLE_SIGNAL);
        assert_eq!(stub.next_reported_line(), "ready");

        let ladder = stub.into_ladder("WorstCaseProbe");
        let started = Instant::now();
        let outcome = ladder.walk_every_rung(never_answers);
        let walked_in = started.elapsed();

        assert!(matches!(outcome, HelperProcessShutdownOutcome::Reaped(_)));
        assert!(
            walked_in < Duration::from_secs(10),
            "the ladder took {walked_in:?}, which leaves no room under the app's watchdog"
        );
    }

    /// Whether `pid` has stopped existing inside `budget`.
    ///
    /// `kill(pid, 0)` and not a wait: a forked worker is not this process's
    /// child, so it is reaped by init rather than here.
    fn a_pid_is_gone_within(pid: libc::pid_t, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            // SAFETY: signal 0 delivers nothing; it only reports reachability.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        }
    }
}
