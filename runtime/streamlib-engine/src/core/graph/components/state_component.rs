// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Condvar, Mutex};
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::processors::ProcessorState;

/// A processor's state together with the signal that it changed.
///
/// One value rather than a state behind one lock and a notification beside it:
/// a waiter reading the state through a separate lock can miss the very
/// transition it waits for, because the writer publishes and notifies in the
/// window between that read and the wait.
pub struct ObservableProcessorState {
    current: Mutex<ProcessorState>,
    changed: Condvar,
    /// Why the processor is in `Error`, in the words of whatever refused it —
    /// a `setup()` that returned an error carries that error's text, so a
    /// caller waiting for the graph is told the reason and not just the state.
    failure_reason: Mutex<Option<String>>,
}

impl ObservableProcessorState {
    /// A processor state starting at `initial`.
    pub fn new(initial: ProcessorState) -> Self {
        Self {
            current: Mutex::new(initial),
            changed: Condvar::new(),
            failure_reason: Mutex::new(None),
        }
    }

    /// The state the processor is in right now.
    pub fn current(&self) -> ProcessorState {
        *self.current.lock()
    }

    /// Move the processor to `state` and wake everything waiting on it.
    pub fn transition_to(&self, state: ProcessorState) {
        *self.current.lock() = state;
        self.changed.notify_all();
    }

    /// Move the processor to `Error`, keeping `reason` for whoever asks why,
    /// and wake everything waiting on it.
    pub fn fail_with(&self, reason: impl Into<String>) {
        let mut current = self.current.lock();
        *self.failure_reason.lock() = Some(reason.into());
        *current = ProcessorState::Error;
        self.changed.notify_all();
    }

    /// Why the processor failed, when it failed through [`Self::fail_with`].
    pub fn failure_reason(&self) -> Option<String> {
        self.failure_reason.lock().clone()
    }

    /// Move the processor to `state`, unless it has already failed.
    ///
    /// A thread that unwinds after `Error` would otherwise report the state it
    /// unwound *into* — `Stopped`, indistinguishable from a clean shutdown —
    /// and lose the only record that anything went wrong. Checked under the
    /// same lock as the write, so a failure landing concurrently is not
    /// overwritten either.
    pub fn transition_to_unless_already_failed(&self, state: ProcessorState) {
        let mut current = self.current.lock();
        if *current == ProcessorState::Error {
            return;
        }
        *current = state;
        self.changed.notify_all();
    }

    /// Block until the processor's `setup` has resolved, one way or the other,
    /// giving up at `deadline`. Returns the state it settled on.
    ///
    /// `Running` means setup returned; `Error` means it failed. A state still
    /// [`before setup completed`] is the deadline expiring.
    ///
    /// [`before setup completed`]: ProcessorState::is_before_setup_completed
    pub fn wait_until_setup_resolved(&self, deadline: Instant) -> ProcessorState {
        let mut current = self.current.lock();
        while current.is_before_setup_completed() {
            if self.changed.wait_until(&mut current, deadline).timed_out() {
                break;
            }
        }
        *current
    }
}

/// Current state of the processor.
pub struct StateComponent(Arc<ObservableProcessorState>);

impl StateComponent {
    /// A processor state starting at `initial`.
    pub fn new(initial: ProcessorState) -> Self {
        Self(Arc::new(ObservableProcessorState::new(initial)))
    }

    /// The state the processor is in right now.
    pub fn current(&self) -> ProcessorState {
        self.0.current()
    }

    /// Move the processor to `state` and wake everything waiting on it.
    pub fn transition_to(&self, state: ProcessorState) {
        self.0.transition_to(state);
    }

    /// Move the processor to `Error` with the reason kept for whoever asks.
    pub fn fail_with(&self, reason: impl Into<String>) {
        self.0.fail_with(reason);
    }

    /// Share the state with a thread that will drive or observe it.
    ///
    /// The shared value outlives the component, which is what lets a waiter
    /// keep watching a processor across a graph write it does not hold a lock
    /// for.
    pub fn clone_inner(&self) -> Arc<ObservableProcessorState> {
        Arc::clone(&self.0)
    }
}

impl JsonSerializableComponent for StateComponent {
    fn json_key(&self) -> &'static str {
        "state"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(self.current().to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A deadline far enough out that reaching it means the wait failed to
    /// observe the transition, rather than that the machine was slow.
    const UNREACHABLE_DEADLINE: Duration = Duration::from_secs(30);

    /// The whole point: the wait ends when the transition happens, not when
    /// the deadline expires. A poll loop would pass the state assertion and
    /// fail this one's timing bound.
    #[test]
    fn a_wait_ends_on_the_transition_rather_than_on_the_deadline() {
        let state = Arc::new(ObservableProcessorState::new(ProcessorState::Idle));

        let transitioning = Arc::clone(&state);
        let transition = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            transitioning.transition_to(ProcessorState::Running);
        });

        let began_waiting = Instant::now();
        let settled = state.wait_until_setup_resolved(began_waiting + UNREACHABLE_DEADLINE);
        let waited = began_waiting.elapsed();
        transition.join().expect("the transition thread panicked");

        assert_eq!(settled, ProcessorState::Running);
        assert!(
            waited < Duration::from_secs(5),
            "waited {waited:?} for a transition that happened after 50ms — the wait is not \
             riding the change signal"
        );
    }

    /// Setup failing has to end the wait too, or a graph with one broken
    /// processor burns the caller's whole timeout before saying so.
    #[test]
    fn a_failed_setup_ends_the_wait() {
        let state = Arc::new(ObservableProcessorState::new(ProcessorState::Idle));

        let failing = Arc::clone(&state);
        std::thread::spawn(move || failing.transition_to(ProcessorState::Error))
            .join()
            .expect("the transition thread panicked");

        assert_eq!(
            state.wait_until_setup_resolved(Instant::now() + UNREACHABLE_DEADLINE),
            ProcessorState::Error
        );
    }

    /// The missed-wakeup case: the transition already happened, so there is no
    /// notification left to receive and the predicate alone has to end the wait.
    #[test]
    fn a_transition_that_already_happened_does_not_hang_the_wait() {
        let state = ObservableProcessorState::new(ProcessorState::Pending);
        state.transition_to(ProcessorState::Running);

        assert_eq!(
            state.wait_until_setup_resolved(Instant::now() + UNREACHABLE_DEADLINE),
            ProcessorState::Running
        );
    }

    /// A failure carries its own words: the state alone says a processor is
    /// broken, the reason says what refused it, and the thread that unwinds
    /// afterwards leaves both in place.
    #[test]
    fn a_failure_keeps_the_reason_it_failed_for() {
        let state = ObservableProcessorState::new(ProcessorState::Pending);
        assert_eq!(state.failure_reason(), None);

        state.fail_with("setup failed: no permission to create a camera");
        state.transition_to_unless_already_failed(ProcessorState::Stopped);

        assert_eq!(state.current(), ProcessorState::Error);
        assert_eq!(
            state.failure_reason().as_deref(),
            Some("setup failed: no permission to create a camera")
        );
        assert_eq!(
            state.wait_until_setup_resolved(Instant::now() + UNREACHABLE_DEADLINE),
            ProcessorState::Error,
            "a failure with a reason ends the wait like one without"
        );
    }

    /// The thread that unwinds after a failure reports `Stopped` on its way
    /// out. It must not bury the failure, or a processor whose `start()` failed
    /// becomes indistinguishable from one that shut down cleanly.
    #[test]
    fn stopping_a_processor_does_not_bury_why_it_stopped() {
        let failed = ObservableProcessorState::new(ProcessorState::Running);
        failed.transition_to(ProcessorState::Error);
        failed.transition_to_unless_already_failed(ProcessorState::Stopped);
        assert_eq!(failed.current(), ProcessorState::Error);

        let clean = ObservableProcessorState::new(ProcessorState::Running);
        clean.transition_to_unless_already_failed(ProcessorState::Stopped);
        assert_eq!(clean.current(), ProcessorState::Stopped);
    }

    /// A processor that never starts gives the caller back the state it is
    /// stuck in, which is what turns the timeout into a diagnostic.
    #[test]
    fn an_expired_deadline_reports_the_state_it_gave_up_in() {
        let state = ObservableProcessorState::new(ProcessorState::Pending);

        let settled = state.wait_until_setup_resolved(Instant::now() + Duration::from_millis(20));

        assert_eq!(settled, ProcessorState::Pending);
        assert!(settled.is_before_setup_completed());
    }
}
