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
}

impl ObservableProcessorState {
    /// A processor state starting at `initial`.
    pub fn new(initial: ProcessorState) -> Self {
        Self {
            current: Mutex::new(initial),
            changed: Condvar::new(),
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
