// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::Duration;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::OwnedFd;

use crate::core::context::{StreamClock, StreamClockSpec};
use crate::iceoryx2::{InputMailboxesInner, OutputWriterInner};

pub const PAUSE_CHECK_INTERVAL: Duration = Duration::from_millis(10);

pub const NO_WAITER_FALLBACK_SLEEP: Duration = Duration::from_millis(100);

/// Longest single wait before a continuous loop re-reads its clock, so a live
/// rescale reaches a wait already under way.
pub const CONTINUOUS_LOOP_CLOCK_RECHECK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSchedulingCallback {
    OnPause,
    OnResume,
    Process,
}

/// Whether a scheduling loop keeps running after what arrived for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSchedulingLoopControl {
    KeepRunning,
    Stop,
}

/// The clock a continuous loop paces on.
pub trait ClockANodeSchedulingLoopPacesOn {
    /// Its time now, in nanoseconds.
    fn now_ns(&self) -> i64;
    /// Machine nanoseconds until it reaches `deadline_ns`, as it runs now;
    /// zero or less once it has.
    fn machine_ns_until(&self, deadline_ns: i64) -> i64;
    /// Whether it advances one step at a time behind the step barrier now.
    fn runs_behind_the_step_barrier(&self) -> bool;
}

impl ClockANodeSchedulingLoopPacesOn for StreamClock {
    fn now_ns(&self) -> i64 {
        StreamClock::now_ns(self)
    }

    fn machine_ns_until(&self, deadline_ns: i64) -> i64 {
        let machine_now_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;
        self.machine_time_ns_reaching(deadline_ns, machine_now_ns)
            .saturating_sub(machine_now_ns)
    }

    fn runs_behind_the_step_barrier(&self) -> bool {
        StreamClock::runs_behind_the_step_barrier(self)
    }
}

pub trait NodeSchedulingLoopHost {
    fn call_node(&mut self, callback: NodeSchedulingCallback, counted_on_the_step_ledger: bool);

    fn note_step_finished(&mut self, step_index: u64);

    fn pause_requested(&self) -> bool;

    fn input_mailboxes(&self) -> Option<Arc<InputMailboxesInner>>;

    /// The writer of the node's output ports. On each pass while the node runs, the loop has
    /// it send each kept last message to a reader that connected after that message.
    fn output_writer(&self) -> Option<Arc<OutputWriterInner>>;

    /// The fd that turns readable when a control arrives, which a reactive
    /// loop waits on beside its input listener. Owned by the host for as long
    /// as the loop runs.
    #[cfg(unix)]
    fn control_arrival_fd(&self) -> Option<std::os::fd::RawFd>;

    /// Take whatever control arrived, without waiting.
    fn take_arrived_control(&mut self) -> NodeSchedulingLoopControl;

    /// Take the control whose arrival turned the control arrival fd readable.
    fn take_the_control_its_fd_announced(&mut self) -> NodeSchedulingLoopControl;

    /// A signal interrupted the loop's wait.
    fn after_a_signal_interrupted_a_wait(&mut self) -> NodeSchedulingLoopControl;

    /// Wait `machine_wait` on the machine's monotonic clock, ending early when
    /// a control arrives, which this takes.
    fn wait_on_the_machine_clock_or_a_control(
        &mut self,
        machine_wait: Duration,
    ) -> NodeSchedulingLoopControl;

    /// The step open once one later than `after` is, waiting up to
    /// `longest_wait` for it; `None` when none opened in that time, a control
    /// arrived, or the clock runs free of the barrier.
    fn wait_for_a_step_opened_after(
        &mut self,
        after: Option<u64>,
        longest_wait: Duration,
    ) -> Option<u64>;
}

// =============================================================================
// Reactive
// =============================================================================

pub fn run_reactive_scheduling_loop(
    node_name_for_logs: &str,
    host: &mut impl NodeSchedulingLoopHost,
) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut waiter: Option<ReactiveLoopFdWaiter> = None;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut waiter_setup_failed = false;

    let mut was_paused = false;

    loop {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let (listener_fd, listener_generation) = match host.input_mailboxes() {
                Some(mailboxes) => (mailboxes.listener_fd(), mailboxes.listener_generation()),
                None => (None, 0),
            };
            refresh_reactive_loop_waiter(
                node_name_for_logs,
                &mut waiter,
                &mut waiter_setup_failed,
                listener_fd,
                listener_generation,
                host.control_arrival_fd(),
            );
        }

        if host.take_arrived_control() == NodeSchedulingLoopControl::Stop {
            tracing::info!("[{node_name_for_logs}] its scheduling loop was stopped");
            return;
        }
        send_each_kept_message_to_a_reader_it_missed(host);

        let is_paused = host.pause_requested();
        if is_paused != was_paused {
            host.call_node(
                if is_paused {
                    NodeSchedulingCallback::OnPause
                } else {
                    NodeSchedulingCallback::OnResume
                },
                true,
            );
            was_paused = is_paused;
        }

        // Every path that is not waking on the listener drains it too, so the
        // next wait wakes only for a notification sent after this pass looked
        // at its ports, never for one it already answered.
        if is_paused {
            if host.wait_on_the_machine_clock_or_a_control(PAUSE_CHECK_INTERVAL)
                == NodeSchedulingLoopControl::Stop
            {
                return;
            }
            drain_input_listener(host);
            continue;
        }

        // A bag can be waiting with no notification left to wake on it: one
        // that arrived during a pause, whose notifications the paused passes
        // drained, or one published after a first link's subscriber existed
        // and before its listener did, which notified nobody. So the loop asks
        // before it waits.
        if !ports_would_return_something(host).unwrap_or(false) {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let waited = match waiter.as_ref() {
                Some(waiter) => match waiter.wait(REACTIVE_WAIT_BOUND) {
                    ReactiveLoopWakeOutcome::Notified => {
                        drain_input_listener(host);
                        NodeSchedulingLoopControl::KeepRunning
                    }
                    ReactiveLoopWakeOutcome::ControlArrived => {
                        if host.take_the_control_its_fd_announced()
                            == NodeSchedulingLoopControl::Stop
                        {
                            return;
                        }
                        continue;
                    }
                    ReactiveLoopWakeOutcome::Interrupted => {
                        if host.after_a_signal_interrupted_a_wait()
                            == NodeSchedulingLoopControl::Stop
                        {
                            return;
                        }
                        continue;
                    }
                    ReactiveLoopWakeOutcome::TimedOut => continue,
                    ReactiveLoopWakeOutcome::Error => wait_without_a_waiter_then_drain(host),
                },
                None => wait_without_a_waiter_then_drain(host),
            };
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let waited = wait_without_a_waiter_then_drain(host);
            if waited == NodeSchedulingLoopControl::Stop {
                return;
            }

            if !ports_would_return_something(host).unwrap_or(true) {
                continue;
            }
        }

        loop {
            host.call_node(NodeSchedulingCallback::Process, true);

            if host.take_arrived_control() == NodeSchedulingLoopControl::Stop {
                tracing::info!("[{node_name_for_logs}] its scheduling loop was stopped");
                return;
            }
            // A long drain still reaches a reader that connected during it.
            send_each_kept_message_to_a_reader_it_missed(host);

            // Drained before the readiness check, so a bag whose notify this
            // clears is one that check sees.
            drain_input_listener(host);

            if !ports_would_return_something(host).unwrap_or(false) {
                break;
            }
        }
    }
}

/// Wait the fallback interval for a control, then drain the listener.
fn wait_without_a_waiter_then_drain(
    host: &mut impl NodeSchedulingLoopHost,
) -> NodeSchedulingLoopControl {
    let waited = host.wait_on_the_machine_clock_or_a_control(NO_WAITER_FALLBACK_SLEEP);
    drain_input_listener(host);
    waited
}

/// Have the node's output ports send each kept last message to a reader that connected after
/// it. Called on every pass of a loop, so a reader that connects while the node writes nothing
/// still reads its value; a paused node sends nothing.
fn send_each_kept_message_to_a_reader_it_missed(host: &impl NodeSchedulingLoopHost) {
    if host.pause_requested() {
        return;
    }
    if let Some(output_writer) = host.output_writer() {
        output_writer.send_each_kept_message_to_a_reader_it_missed();
    }
}

fn ports_would_return_something(host: &impl NodeSchedulingLoopHost) -> Option<bool> {
    host.input_mailboxes()
        .map(|mailboxes| mailboxes.any_port_has_data())
}

fn drain_input_listener(host: &impl NodeSchedulingLoopHost) {
    if let Some(mailboxes) = host.input_mailboxes() {
        mailboxes.drain_listener();
    }
}

// =============================================================================
// Continuous
// =============================================================================

/// Why a continuous loop left the way it was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuousLoopLeftItsMode {
    Stopped,
    /// The stream clock moved behind the step barrier or out of it.
    TheClockChangedHowItAdvances,
}

/// How a continuous loop's wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuousLoopWaitOutcome {
    DeadlineReached,
    Stopped,
    /// What the wait was told to stop early for held.
    StoppedWaitingEarly,
}

pub fn run_continuous_scheduling_loop_on_real_time(
    host: &mut impl NodeSchedulingLoopHost,
    interval_ms: u32,
) {
    let machine_clock = StreamClock::starting_now(StreamClockSpec::Real);
    let mut was_paused = false;
    let why_the_loop_left = run_continuous_scheduling_on_the_interval_grid(
        host,
        &machine_clock,
        interval_ms,
        &mut was_paused,
        || false,
    );
    debug_assert_eq!(why_the_loop_left, ContinuousLoopLeftItsMode::Stopped);
}

pub fn run_continuous_scheduling_loop_on_its_stream_clock(
    host: &mut impl NodeSchedulingLoopHost,
    stream_clock: &impl ClockANodeSchedulingLoopPacesOn,
    interval_ms: u32,
) {
    let mut was_paused = false;
    let mut last_step_handled: Option<u64> = None;
    loop {
        let why_the_loop_left_its_mode = if stream_clock.runs_behind_the_step_barrier() {
            run_continuous_scheduling_in_steps(
                host,
                stream_clock,
                interval_ms,
                &mut was_paused,
                &mut last_step_handled,
            )
        } else {
            run_continuous_scheduling_on_the_interval_grid(
                host,
                stream_clock,
                interval_ms,
                &mut was_paused,
                || stream_clock.runs_behind_the_step_barrier(),
            )
        };
        if why_the_loop_left_its_mode == ContinuousLoopLeftItsMode::Stopped {
            return;
        }
    }
}

fn run_continuous_scheduling_on_the_interval_grid(
    host: &mut impl NodeSchedulingLoopHost,
    clock_it_paces_on: &impl ClockANodeSchedulingLoopPacesOn,
    interval_ms: u32,
    was_paused: &mut bool,
    leaves_when: impl Fn() -> bool,
) -> ContinuousLoopLeftItsMode {
    let interval_ns = continuous_interval_ns(interval_ms);
    let mut next_deadline_ns = clock_it_paces_on.now_ns();

    loop {
        match wait_for_a_deadline_or_a_control(
            host,
            clock_it_paces_on,
            next_deadline_ns,
            &leaves_when,
        ) {
            ContinuousLoopWaitOutcome::Stopped => return ContinuousLoopLeftItsMode::Stopped,
            ContinuousLoopWaitOutcome::StoppedWaitingEarly => {
                return ContinuousLoopLeftItsMode::TheClockChangedHowItAdvances;
            }
            ContinuousLoopWaitOutcome::DeadlineReached => {}
        }

        let is_paused = host.pause_requested();
        if is_paused != *was_paused {
            host.call_node(
                if is_paused {
                    NodeSchedulingCallback::OnPause
                } else {
                    NodeSchedulingCallback::OnResume
                },
                false,
            );
            *was_paused = is_paused;
        }

        if is_paused {
            if host.wait_on_the_machine_clock_or_a_control(PAUSE_CHECK_INTERVAL)
                == NodeSchedulingLoopControl::Stop
            {
                return ContinuousLoopLeftItsMode::Stopped;
            }
            if leaves_when() {
                return ContinuousLoopLeftItsMode::TheClockChangedHowItAdvances;
            }
            next_deadline_ns = clock_it_paces_on.now_ns();
            continue;
        }

        host.call_node(NodeSchedulingCallback::Process, false);

        next_deadline_ns =
            next_continuous_deadline_ns(next_deadline_ns, interval_ns, clock_it_paces_on.now_ns());
    }
}

fn run_continuous_scheduling_in_steps(
    host: &mut impl NodeSchedulingLoopHost,
    stream_clock: &impl ClockANodeSchedulingLoopPacesOn,
    interval_ms: u32,
    was_paused: &mut bool,
    last_step_handled: &mut Option<u64>,
) -> ContinuousLoopLeftItsMode {
    let interval_ns = continuous_interval_ns(interval_ms);
    let mut next_deadline_ns: Option<i64> = None;

    loop {
        if host.take_arrived_control() == NodeSchedulingLoopControl::Stop {
            return ContinuousLoopLeftItsMode::Stopped;
        }
        send_each_kept_message_to_a_reader_it_missed(host);
        let Some(step_index) =
            host.wait_for_a_step_opened_after(*last_step_handled, CONTINUOUS_LOOP_CLOCK_RECHECK)
        else {
            if !stream_clock.runs_behind_the_step_barrier() {
                return ContinuousLoopLeftItsMode::TheClockChangedHowItAdvances;
            }
            continue;
        };

        let is_paused = host.pause_requested();
        if is_paused != *was_paused {
            host.call_node(
                if is_paused {
                    NodeSchedulingCallback::OnPause
                } else {
                    NodeSchedulingCallback::OnResume
                },
                true,
            );
            *was_paused = is_paused;
        }

        let step_ns = stream_clock.now_ns();
        let deadline_ns = *next_deadline_ns.get_or_insert(step_ns);
        if !is_paused && deadline_ns <= step_ns {
            host.call_node(NodeSchedulingCallback::Process, true);
            next_deadline_ns = Some(next_continuous_deadline_ns(
                deadline_ns,
                interval_ns,
                step_ns,
            ));
        }
        host.note_step_finished(step_index);
        *last_step_handled = Some(step_index);
    }
}

pub fn continuous_interval_ns(interval_ms: u32) -> i64 {
    if interval_ms > 0 {
        i64::from(interval_ms) * 1_000_000
    } else {
        100_000
    }
}

pub fn next_continuous_deadline_ns(
    previous_deadline_ns: i64,
    interval_ns: i64,
    now_ns: i64,
) -> i64 {
    let intervals_passed = now_ns.saturating_sub(previous_deadline_ns).max(0) / interval_ns;
    previous_deadline_ns.saturating_add(
        intervals_passed
            .saturating_add(1)
            .saturating_mul(interval_ns),
    )
}

/// Wait until `clock` reaches `deadline_ns`, a control stops the loop, or
/// `stops_waiting_early` holds.
pub(crate) fn wait_for_a_deadline_or_a_control(
    host: &mut impl NodeSchedulingLoopHost,
    clock: &impl ClockANodeSchedulingLoopPacesOn,
    deadline_ns: i64,
    stops_waiting_early: impl Fn() -> bool,
) -> ContinuousLoopWaitOutcome {
    loop {
        if host.take_arrived_control() == NodeSchedulingLoopControl::Stop {
            return ContinuousLoopWaitOutcome::Stopped;
        }
        send_each_kept_message_to_a_reader_it_missed(host);
        if stops_waiting_early() {
            return ContinuousLoopWaitOutcome::StoppedWaitingEarly;
        }
        let machine_ns_to_go = clock.machine_ns_until(deadline_ns);
        if machine_ns_to_go <= 0 {
            return ContinuousLoopWaitOutcome::DeadlineReached;
        }
        let machine_wait_before_recheck =
            Duration::from_nanos(machine_ns_to_go as u64).min(CONTINUOUS_LOOP_CLOCK_RECHECK);
        if host.wait_on_the_machine_clock_or_a_control(machine_wait_before_recheck)
            == NodeSchedulingLoopControl::Stop
        {
            return ContinuousLoopWaitOutcome::Stopped;
        }
    }
}

// =============================================================================
// The reactive loop's waiter
// =============================================================================

/// Outcome of one [`ReactiveLoopFdWaiter::wait`] call.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReactiveLoopWakeOutcome {
    /// The listener fd turned readable: at least one upstream notify arrived.
    Notified,
    /// The control arrival fd turned readable.
    ControlArrived,
    /// A signal interrupted the wait (`EINTR`).
    Interrupted,
    /// Nothing fired within the bounded wait; the caller comes round to check
    /// whether its listener is still the one the mailboxes hold.
    TimedOut,
    /// The wait returned an unrecoverable error.
    Error,
}

/// How long a reactive wait blocks before returning empty-handed, so a loop
/// whose listener was replaced while it slept comes round to rebuild its
/// waiter rather than sleeping on a dead fd.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) const REACTIVE_WAIT_BOUND: Duration = Duration::from_millis(500);

/// Keep the waiter on the listener the mailboxes hold now.
///
/// A listener goes with a destination's last inbound link and comes back with
/// the next one, and a readiness queue never learns of the new fd on its own —
/// a closed fd simply leaves it. So a waiter built for an earlier generation,
/// or for a listener that is gone, is released, and one is built for the
/// current listener when there is one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn refresh_reactive_loop_waiter(
    node_name_for_logs: &str,
    waiter: &mut Option<ReactiveLoopFdWaiter>,
    waiter_setup_failed: &mut bool,
    listener_fd: Option<i32>,
    listener_generation: u64,
    control_arrival_fd: Option<std::os::fd::RawFd>,
) {
    let registered_listener_is_gone = waiter.as_ref().is_some_and(|registered| {
        listener_fd.is_none() || registered.listener_generation != listener_generation
    });
    if registered_listener_is_gone {
        *waiter = None;
    }
    if waiter.is_some() || *waiter_setup_failed {
        return;
    }
    let Some(fd) = listener_fd else {
        return;
    };
    match ReactiveLoopFdWaiter::new(fd, listener_generation, control_arrival_fd) {
        Ok(built) => *waiter = Some(built),
        Err(setup_failure) => {
            tracing::warn!(
                "[{node_name_for_logs}] its reactive waiter could not be set up, so it \
                 waits on the fallback interval instead: {setup_failure}"
            );
            *waiter_setup_failed = true;
        }
    }
}

/// Which of the waiter's two fds a readiness-queue registration watches.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReactiveLoopFdRegistration {
    InputListener,
    ControlArrival,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ReactiveLoopFdRegistration {
    /// The value carried in the kernel's per-registration user data.
    fn user_data(self) -> u64 {
        match self {
            Self::InputListener => 0,
            Self::ControlArrival => 1,
        }
    }

    fn from_user_data(user_data: u64) -> Self {
        if user_data == Self::ControlArrival.user_data() {
            Self::ControlArrival
        } else {
            Self::InputListener
        }
    }
}

/// Which registrations one wait found readable; neither means it timed out.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Default, Clone, Copy)]
struct ReactiveLoopReadableRegistrations {
    input_listener: bool,
    control_arrival: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ReactiveLoopReadableRegistrations {
    fn mark_readable(&mut self, registration: ReactiveLoopFdRegistration) {
        match registration {
            ReactiveLoopFdRegistration::InputListener => self.input_listener = true,
            ReactiveLoopFdRegistration::ControlArrival => self.control_arrival = true,
        }
    }
}

/// The reactive loop's readiness queue — an epoll set on Linux, a kqueue on
/// macOS. Level-triggered on both, so an undrained fd wakes every wait.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct ReactiveLoopReadinessQueue {
    readiness_queue_fd: OwnedFd,
}

/// The waiter a reactive loop blocks in: its readiness queue watching the
/// iceoryx2 listener fd and, when the host has one, its control arrival fd.
///
/// Registers both fds without owning them; their owners outlive the loop.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct ReactiveLoopFdWaiter {
    readiness_queue: ReactiveLoopReadinessQueue,
    /// Which of the destination's listeners this waiter registered, by the
    /// generation the mailboxes assigned it. A listener created after the last
    /// inbound link went away is a new fd the readiness queue never saw.
    listener_generation: u64,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ReactiveLoopFdWaiter {
    pub(crate) fn new(
        listener_fd: std::os::fd::RawFd,
        listener_generation: u64,
        control_arrival_fd: Option<std::os::fd::RawFd>,
    ) -> std::io::Result<Self> {
        let readiness_queue = ReactiveLoopReadinessQueue::create()?;
        readiness_queue
            .register_readable_fd(listener_fd, ReactiveLoopFdRegistration::InputListener)?;
        if let Some(control_arrival_fd) = control_arrival_fd {
            readiness_queue.register_readable_fd(
                control_arrival_fd,
                ReactiveLoopFdRegistration::ControlArrival,
            )?;
        }
        Ok(Self {
            readiness_queue,
            listener_generation,
        })
    }

    /// Block until a registered fd is readable, a signal interrupts the call,
    /// or `wait_bound` elapses with nothing to report.
    pub(crate) fn wait(&self, wait_bound: Duration) -> ReactiveLoopWakeOutcome {
        match self.readiness_queue.wait_for_readable(wait_bound) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                ReactiveLoopWakeOutcome::Interrupted
            }
            Err(e) => {
                tracing::warn!("reactive loop wait failed: {}", e);
                ReactiveLoopWakeOutcome::Error
            }
            // A control outranks a notify reported by the same wait, so a stop
            // ends the loop before it drains one more frame.
            Ok(readable) if readable.control_arrival => ReactiveLoopWakeOutcome::ControlArrived,
            Ok(readable) if readable.input_listener => ReactiveLoopWakeOutcome::Notified,
            Ok(_) => ReactiveLoopWakeOutcome::TimedOut,
        }
    }
}

#[cfg(target_os = "linux")]
impl ReactiveLoopReadinessQueue {
    fn create() -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        // SAFETY: epoll_create1 returns -1 on failure; checked below.
        let raw_epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if raw_epoll_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw_epoll_fd was just opened here and nothing else owns it.
        let readiness_queue_fd = unsafe { OwnedFd::from_raw_fd(raw_epoll_fd) };
        Ok(Self { readiness_queue_fd })
    }

    fn register_readable_fd(
        &self,
        watched_fd: std::os::fd::RawFd,
        registration: ReactiveLoopFdRegistration,
    ) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: registration.user_data(),
        };
        // SAFETY: epoll_ctl with EPOLL_CTL_ADD takes a pointer to a valid
        // epoll_event for the duration of the call.
        let result = unsafe {
            libc::epoll_ctl(
                self.readiness_queue_fd.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                watched_fd,
                &mut event,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn wait_for_readable(
        &self,
        wait_bound: Duration,
    ) -> std::io::Result<ReactiveLoopReadableRegistrations> {
        use std::os::fd::AsRawFd;
        let mut events = [libc::epoll_event { events: 0, u64: 0 }; 2];
        let timeout_ms = wait_bound.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: epoll_wait writes up to events.len() events into the buffer.
        let ready_count = unsafe {
            libc::epoll_wait(
                self.readiness_queue_fd.as_raw_fd(),
                events.as_mut_ptr(),
                events.len() as i32,
                timeout_ms,
            )
        };
        if ready_count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut readable = ReactiveLoopReadableRegistrations::default();
        for event in &events[..ready_count as usize] {
            readable.mark_readable(ReactiveLoopFdRegistration::from_user_data(event.u64));
        }
        Ok(readable)
    }
}

#[cfg(target_os = "macos")]
impl ReactiveLoopReadinessQueue {
    fn create() -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: kqueue returns -1 on failure; checked below.
        let raw_kqueue_fd = unsafe { libc::kqueue() };
        if raw_kqueue_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw_kqueue_fd was just opened here and nothing else owns it.
        let readiness_queue_fd = unsafe { OwnedFd::from_raw_fd(raw_kqueue_fd) };
        // Darwin has no `kqueue1`, so close-on-exec is set before the fd is used.
        // SAFETY: fcntl on a live fd this function owns.
        if unsafe {
            libc::fcntl(
                readiness_queue_fd.as_raw_fd(),
                libc::F_SETFD,
                libc::FD_CLOEXEC,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { readiness_queue_fd })
    }

    /// No `EV_CLEAR`, which is what keeps the registration level-triggered.
    fn register_readable_fd(
        &self,
        watched_fd: std::os::fd::RawFd,
        registration: ReactiveLoopFdRegistration,
    ) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let change = libc::kevent {
            ident: watched_fd as libc::uintptr_t,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD,
            fflags: 0,
            data: 0,
            udata: std::ptr::without_provenance_mut(registration.user_data() as usize),
        };
        // SAFETY: one valid change in, no event slots out; a failed
        // registration returns -1 when the event list is empty.
        let result = unsafe {
            libc::kevent(
                self.readiness_queue_fd.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn wait_for_readable(
        &self,
        wait_bound: Duration,
    ) -> std::io::Result<ReactiveLoopReadableRegistrations> {
        use std::os::fd::AsRawFd;
        let timeout = libc::timespec {
            tv_sec: wait_bound.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
            tv_nsec: wait_bound.subsec_nanos() as libc::c_long,
        };
        // SAFETY: an all-zero `kevent` is a valid out-slot.
        let mut events: [libc::kevent; 2] = unsafe { std::mem::zeroed() };
        // SAFETY: kevent writes up to events.len() events into the buffer; the
        // timeout is a valid stack slot.
        let ready_count = unsafe {
            libc::kevent(
                self.readiness_queue_fd.as_raw_fd(),
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                &timeout,
            )
        };
        if ready_count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut readable = ReactiveLoopReadableRegistrations::default();
        for event in &events[..ready_count as usize] {
            if event.flags & libc::EV_ERROR != 0 {
                return Err(std::io::Error::from_raw_os_error(event.data as i32));
            }
            readable.mark_readable(ReactiveLoopFdRegistration::from_user_data(
                event.udata.addr() as u64,
            ));
        }
        Ok(readable)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use iceoryx2::prelude::*;
    use std::os::fd::AsRawFd;

    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/scheduling-loop/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    fn listener_fd_of(
        listener: &iceoryx2::port::listener::Listener<ipc_threadsafe::Service>,
    ) -> i32 {
        // SAFETY: every caller uses the fd only while `listener` is alive.
        unsafe { listener.file_descriptor().native_handle() }
    }

    /// A destination that loses its last inbound link drops its listener, and
    /// the next connect creates a new one the readiness queue never saw. The runner
    /// keeps its waiter on the listener the mailboxes hold now, so a frame on
    /// the reconnected link wakes it. Revert lock: register the listener once
    /// before the loop and the second notify below wakes nothing.
    #[test]
    fn the_waiter_follows_a_listener_replaced_after_the_last_link_went_away() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let open_event_service = |suffix: &str| {
            node.service_builder(&ServiceName::new(&unique_suffix(suffix)).unwrap())
                .event()
                .max_notifiers(1)
                .max_listeners(1)
                .open_or_create()
                .unwrap()
        };
        let mut waiter = None;
        let mut waiter_setup_failed = false;

        let first = open_event_service("replaced-first");
        let first_listener =
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&first)
                .unwrap();
        let first_fd = listener_fd_of(&first_listener);
        refresh_reactive_loop_waiter(
            "Preactive",
            &mut waiter,
            &mut waiter_setup_failed,
            Some(first_fd),
            1,
            None,
        );
        let first_readiness_queue_fd = waiter
            .as_ref()
            .expect("a waiter for the first listener")
            .readiness_queue
            .readiness_queue_fd
            .as_raw_fd();

        refresh_reactive_loop_waiter(
            "Preactive",
            &mut waiter,
            &mut waiter_setup_failed,
            Some(first_fd),
            1,
            None,
        );
        assert_eq!(
            waiter
                .as_ref()
                .map(|registered| registered.readiness_queue.readiness_queue_fd.as_raw_fd()),
            Some(first_readiness_queue_fd),
            "the same listener keeps the same waiter"
        );

        // The last link goes, and the listener with it.
        drop(first_listener);
        refresh_reactive_loop_waiter(
            "Preactive",
            &mut waiter,
            &mut waiter_setup_failed,
            None,
            1,
            None,
        );
        assert!(
            waiter.is_none(),
            "a waiter on a dropped listener is released"
        );

        // A reconnect creates a new listener, on a fresh notify service.
        let second = open_event_service("replaced-second");
        let second_listener =
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&second)
                .unwrap();
        let second_fd = listener_fd_of(&second_listener);
        refresh_reactive_loop_waiter(
            "Preactive",
            &mut waiter,
            &mut waiter_setup_failed,
            Some(second_fd),
            2,
            None,
        );
        let waiter = waiter.expect("a waiter for the new listener");
        let notifier = second.notifier_builder().create().unwrap();

        notifier.notify().unwrap();
        let outcome = waiter.wait(std::time::Duration::ZERO);
        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Notified),
            "a notify on the reconnected link must wake the runner; got {outcome:?}"
        );
        second_listener.try_wait(|_| {}).unwrap();
    }
}
