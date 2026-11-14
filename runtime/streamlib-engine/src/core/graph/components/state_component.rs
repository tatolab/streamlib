// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Condvar, Mutex};
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::error::{Error, PrivacyGatedCaptureDevice};
use crate::core::graph::ProcessorUniqueId;
use crate::core::processors::ProcessorState;
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};

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
    /// The capture device whose access was never asked for, when that is why
    /// `setup()` refused.
    capture_device_access_never_asked_for: Mutex<Option<PrivacyGatedCaptureDevice>>,
    helper_process_restarts: Mutex<HelperProcessRestartRecord>,
    /// What the processor's `process()` raised or returned as an error the
    /// last time it ran, while it has not run cleanly since.
    process_raising_because: Mutex<Option<String>>,
    /// Why the node, running, is not ready, in its own words, while it says
    /// so; the runtime keeps its outputs flowing meanwhile.
    not_ready_because: Mutex<Option<String>>,
    state_changes_published_as: Mutex<Option<ProcessorStateChangesPublishedOnItsStreamsTopic>>,
}

/// The processor and stream each state change is published as a
/// `ProcessorStateDidChange` of.
struct ProcessorStateChangesPublishedOnItsStreamsTopic {
    processor_id: ProcessorUniqueId,
    stream: LoadedStreamIdentity,
}

/// How often a helper-placed processor's helper process has been started again
/// after dying on its own, and why it last died.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelperProcessRestartRecord {
    /// New helper processes started since the stream loaded.
    pub restart_count: u32,
    /// The last death's exit and the end of its standard error.
    pub last_exit_reason: Option<String>,
}

impl ObservableProcessorState {
    /// A processor state starting at `initial`.
    pub fn new(initial: ProcessorState) -> Self {
        Self {
            current: Mutex::new(initial),
            changed: Condvar::new(),
            failure_reason: Mutex::new(None),
            capture_device_access_never_asked_for: Mutex::new(None),
            helper_process_restarts: Mutex::new(HelperProcessRestartRecord::default()),
            process_raising_because: Mutex::new(None),
            not_ready_because: Mutex::new(None),
            state_changes_published_as: Mutex::new(None),
        }
    }

    /// From now on, publish each change of the state as a `ProcessorStateDidChange` of
    /// `processor_id` on `stream`'s topic.
    pub fn publish_each_change_as_a_processor_of(
        &self,
        processor_id: ProcessorUniqueId,
        stream: LoadedStreamIdentity,
    ) {
        *self.state_changes_published_as.lock() =
            Some(ProcessorStateChangesPublishedOnItsStreamsTopic {
                processor_id,
                stream,
            });
    }

    /// Write `new_state` over `current`, which the caller holds locked, wake every waiter, and
    /// publish the change under that lock so the changes publish in the order they were made.
    fn write_the_state_and_publish_the_change(
        &self,
        current: &mut ProcessorState,
        new_state: ProcessorState,
    ) {
        let old_state = std::mem::replace(current, new_state);
        if !new_state.holds_a_not_ready_report() {
            *self.not_ready_because.lock() = None;
        }
        self.changed.notify_all();
        if old_state == new_state {
            return;
        }
        if let Some(published_as) = &*self.state_changes_published_as.lock() {
            published_as.stream.publish_on_this_streams_topic(
                RuntimeEvent::ProcessorStateDidChange {
                    processor_id: published_as.processor_id.clone(),
                    old_state,
                    new_state,
                },
            );
        }
    }

    /// The state the processor is in right now.
    pub fn current(&self) -> ProcessorState {
        *self.current.lock()
    }

    /// Move the processor to `state` and wake everything waiting on it.
    pub fn transition_to(&self, state: ProcessorState) {
        self.write_the_state_and_publish_the_change(&mut self.current.lock(), state);
    }

    /// Move the processor to `Error`, keeping `reason` for whoever asks why,
    /// and wake everything waiting on it.
    pub fn fail_with(&self, reason: impl Into<String>) {
        self.fail_with_reason_and_capture_device(reason.into(), None);
    }

    /// Move the processor to `Error` because its `setup()` returned
    /// `setup_refusal`, keeping the refusal's text and the capture device it
    /// names when nobody had asked for that device's access, and wake
    /// everything waiting on it.
    pub fn fail_with_the_setup_refusal(&self, setup_refusal: &Error) {
        let capture_device_access_never_asked_for = match setup_refusal {
            Error::CaptureDeviceAccessNeverAskedFor { device, .. } => Some(*device),
            _ => None,
        };
        self.fail_with_reason_and_capture_device(
            format!("setup failed: {setup_refusal}"),
            capture_device_access_never_asked_for,
        );
    }

    fn fail_with_reason_and_capture_device(
        &self,
        reason: String,
        capture_device_access_never_asked_for: Option<PrivacyGatedCaptureDevice>,
    ) {
        let mut current = self.current.lock();
        *self.failure_reason.lock() = Some(reason);
        *self.capture_device_access_never_asked_for.lock() = capture_device_access_never_asked_for;
        self.write_the_state_and_publish_the_change(&mut current, ProcessorState::Error);
    }

    /// Why the processor failed, when it failed through [`Self::fail_with`].
    pub fn failure_reason(&self) -> Option<String> {
        self.failure_reason.lock().clone()
    }

    /// The capture device whose access nobody had asked for, when that is why
    /// the processor's `setup()` refused.
    pub fn capture_device_access_never_asked_for(&self) -> Option<PrivacyGatedCaptureDevice> {
        *self.capture_device_access_never_asked_for.lock()
    }

    /// Note that the processor's helper process died on its own, for
    /// `exit_reason`.
    pub fn note_a_helper_process_death(&self, exit_reason: String) {
        self.helper_process_restarts.lock().last_exit_reason = Some(exit_reason);
    }

    /// Note that a new helper process was started in place of one that died.
    pub fn note_a_helper_process_restart(&self) {
        self.helper_process_restarts.lock().restart_count += 1;
    }

    /// How often the helper process was restarted, and why it last died.
    pub fn helper_process_restart_record(&self) -> HelperProcessRestartRecord {
        self.helper_process_restarts.lock().clone()
    }

    /// Note how the processor's `process()` last ended: `Some` with what it
    /// raised, or `None` once it ran cleanly again. The processor keeps running
    /// either way.
    pub fn note_why_process_is_raising(&self, raising_because: Option<String>) {
        *self.process_raising_because.lock() = raising_because;
    }

    /// Note that `process()` raised, keeping the words of the first raise of a
    /// run of them; `describe_the_raise` is called only when no raise is held.
    pub fn note_that_process_raised(&self, describe_the_raise: impl FnOnce() -> String) {
        let mut process_raising_because = self.process_raising_because.lock();
        if process_raising_because.is_none() {
            *process_raising_because = Some(describe_the_raise());
        }
    }

    /// Note that `process()` ran cleanly, ending a run of raises.
    pub fn note_that_process_ran_cleanly(&self) {
        let mut process_raising_because = self.process_raising_because.lock();
        if process_raising_because.is_some() {
            *process_raising_because = None;
        }
    }

    /// What `process()` raised, while it has not run cleanly since.
    pub fn why_process_is_raising(&self) -> Option<String> {
        self.process_raising_because.lock().clone()
    }

    /// Note whether the node, running, is ready: `Some` with why it is not, or
    /// `None` once it is. Kept only while the processor sets up, runs or is
    /// paused.
    pub fn note_why_the_node_is_not_ready(&self, not_ready_because: Option<String>) {
        let current = self.current.lock();
        let mut held = self.not_ready_because.lock();
        *held = if current.holds_a_not_ready_report() {
            not_ready_because
        } else {
            None
        };
    }

    /// Why the node, running, is not ready, while it says so.
    pub fn why_the_node_is_not_ready(&self) -> Option<String> {
        self.not_ready_because.lock().clone()
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
        self.write_the_state_and_publish_the_change(&mut current, state);
    }

    /// Move the processor to `state` only while it is in `only_from`, and wake
    /// everything waiting on it; whether it moved.
    pub fn transition_to_only_from(
        &self,
        only_from: ProcessorState,
        state: ProcessorState,
    ) -> bool {
        let mut current = self.current.lock();
        if *current != only_from {
            return false;
        }
        self.write_the_state_and_publish_the_change(&mut current, state);
        true
    }

    /// Block until the processor's `setup` has resolved, one way or the other,
    /// giving up at `deadline`. Returns the state it settled on.
    ///
    /// `Running` means setup returned; `Error` means it failed. A state still
    /// [`before setup completed`] is the deadline expiring. A helper-placed
    /// processor that reads `Restarting` is waited on until its next start
    /// resolves.
    ///
    /// [`before setup completed`]: ProcessorState::is_before_setup_completed
    pub fn wait_until_setup_resolved(&self, deadline: Instant) -> ProcessorState {
        let mut current = self.current.lock();
        while current.is_before_setup_completed() || *current == ProcessorState::Restarting {
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

    /// Move the processor to `state` only while it is in `only_from`; whether
    /// it moved.
    pub fn transition_to_only_from(
        &self,
        only_from: ProcessorState,
        state: ProcessorState,
    ) -> bool {
        self.0.transition_to_only_from(only_from, state)
    }

    /// [`ObservableProcessorState::publish_each_change_as_a_processor_of`].
    pub fn publish_each_change_as_a_processor_of(
        &self,
        processor_id: ProcessorUniqueId,
        stream: LoadedStreamIdentity,
    ) {
        self.0
            .publish_each_change_as_a_processor_of(processor_id, stream);
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

/// The restart record of a helper-placed processor, rendered beside its
/// state: `restart_count` and `last_exit_reason`.
pub struct HelperProcessRestartsComponent(pub Arc<ObservableProcessorState>);

impl JsonSerializableComponent for HelperProcessRestartsComponent {
    fn json_key(&self) -> &'static str {
        "helper_process_restarts"
    }

    fn to_json(&self) -> JsonValue {
        let record = self.0.helper_process_restart_record();
        serde_json::json!({
            "restart_count": record.restart_count,
            "last_exit_reason": record.last_exit_reason,
        })
    }
}

/// Why a processor in `Error` failed, rendered beside its state as
/// `failed_because`: the failure's own words, or `null` while it has not failed.
pub struct ProcessorFailureReasonComponent(pub Arc<ObservableProcessorState>);

impl JsonSerializableComponent for ProcessorFailureReasonComponent {
    fn json_key(&self) -> &'static str {
        "failed_because"
    }

    fn to_json(&self) -> JsonValue {
        if self.0.current() != ProcessorState::Error {
            return JsonValue::Null;
        }
        serde_json::json!(self.0.failure_reason())
    }
}

/// What a processor's `process()` keeps raising, rendered beside its state as
/// `process_raising_because`: the raise's own words while `process()` has not
/// run cleanly since, or `null`.
pub struct ProcessRaisingBecauseComponent(pub Arc<ObservableProcessorState>);

impl JsonSerializableComponent for ProcessRaisingBecauseComponent {
    fn json_key(&self) -> &'static str {
        "process_raising_because"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(self.0.why_process_is_raising())
    }
}

/// Why a running node is not ready, rendered beside its state as
/// `not_ready_because`: the node's own words while it says so, or `null`.
pub struct NodeNotReadyBecauseComponent(pub Arc<ObservableProcessorState>);

impl JsonSerializableComponent for NodeNotReadyBecauseComponent {
    fn json_key(&self) -> &'static str {
        "not_ready_because"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(self.0.why_the_node_is_not_ready())
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

    /// A node's not-ready reason renders from `setup()` on until it reports
    /// ready, and a processor that stops forgets it.
    #[test]
    fn a_not_ready_reason_is_held_while_the_node_runs_and_forgotten_once_it_stops() {
        let state = Arc::new(ObservableProcessorState::new(ProcessorState::Idle));
        let rendered = NodeNotReadyBecauseComponent(Arc::clone(&state));
        assert_eq!(rendered.to_json(), JsonValue::Null);

        state.note_why_the_node_is_not_ready(Some("`riv_path` is not set".to_string()));
        state.transition_to(ProcessorState::Running);
        assert_eq!(
            rendered.to_json(),
            serde_json::json!("`riv_path` is not set"),
            "a report made from `setup()` survives into the run"
        );
        state.note_why_the_node_is_not_ready(None);

        state.note_why_the_node_is_not_ready(Some("`riv_path` is not set".to_string()));
        assert_eq!(
            rendered.to_json(),
            serde_json::json!("`riv_path` is not set")
        );

        state.transition_to(ProcessorState::Paused);
        assert_eq!(
            rendered.to_json(),
            serde_json::json!("`riv_path` is not set")
        );

        state.note_why_the_node_is_not_ready(None);
        assert_eq!(rendered.to_json(), JsonValue::Null);

        state.note_why_the_node_is_not_ready(Some("`riv_path` is not set".to_string()));
        state.transition_to(ProcessorState::Stopped);
        assert_eq!(rendered.to_json(), JsonValue::Null);

        state.note_why_the_node_is_not_ready(Some("`riv_path` is not set".to_string()));
        assert_eq!(
            rendered.to_json(),
            JsonValue::Null,
            "a processor that does not run holds no not-ready reason"
        );
    }

    /// A setup that refused a capture device nobody asked access for keeps the
    /// device beside its words; any other refusal names no device.
    #[test]
    fn a_setup_refusing_a_capture_device_never_asked_for_keeps_the_device() {
        let waiting_for_the_camera = ObservableProcessorState::new(ProcessorState::Pending);
        waiting_for_the_camera.fail_with_the_setup_refusal(
            &Error::CaptureDeviceAccessNeverAskedFor {
                device: PrivacyGatedCaptureDevice::Camera,
                refusal: "Camera access has not been asked for yet".to_owned(),
            },
        );
        assert_eq!(waiting_for_the_camera.current(), ProcessorState::Error);
        assert_eq!(
            waiting_for_the_camera.capture_device_access_never_asked_for(),
            Some(PrivacyGatedCaptureDevice::Camera)
        );
        assert_eq!(
            waiting_for_the_camera.failure_reason().as_deref(),
            Some("setup failed: Camera access has not been asked for yet")
        );

        let refused_otherwise = ObservableProcessorState::new(ProcessorState::Pending);
        refused_otherwise
            .fail_with_the_setup_refusal(&Error::Configuration("no such camera".to_owned()));
        assert_eq!(refused_otherwise.current(), ProcessorState::Error);
        assert_eq!(
            refused_otherwise.capture_device_access_never_asked_for(),
            None
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

    /// A pause or a resume moves only a processor whose setup ran: one still
    /// in setup, or one that failed, keeps the state it is in.
    #[test]
    fn a_transition_only_from_a_state_leaves_every_other_state_alone() {
        let running = ObservableProcessorState::new(ProcessorState::Running);
        assert!(running.transition_to_only_from(ProcessorState::Running, ProcessorState::Paused));
        assert_eq!(running.current(), ProcessorState::Paused);

        for kept in [
            ProcessorState::Idle,
            ProcessorState::Error,
            ProcessorState::Stopping,
        ] {
            let state = ObservableProcessorState::new(kept);
            assert!(
                !state.transition_to_only_from(ProcessorState::Running, ProcessorState::Paused)
            );
            assert_eq!(state.current(), kept);
        }
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

    /// Readiness waits through a helper restart for the state the next start
    /// settles on.
    #[test]
    fn a_restarting_processor_is_waited_on_until_its_next_start_resolves() {
        let state = Arc::new(ObservableProcessorState::new(ProcessorState::Restarting));
        let restarter = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                state.transition_to(ProcessorState::Running);
            })
        };

        let settled = state.wait_until_setup_resolved(Instant::now() + Duration::from_secs(5));
        restarter.join().expect("the restarting thread finishes");

        assert_eq!(settled, ProcessorState::Running);
    }

    /// `graph` renders a helper-placed processor's restarts beside its state,
    /// from the same shared state the lifecycle thread writes.
    #[test]
    fn the_restart_record_renders_its_count_and_the_last_exit_reason() {
        let state = Arc::new(ObservableProcessorState::new(ProcessorState::Running));
        let rendered = HelperProcessRestartsComponent(Arc::clone(&state));
        assert_eq!(
            rendered.to_json(),
            serde_json::json!({"restart_count": 0, "last_exit_reason": null})
        );

        state.note_a_helper_process_death("its helper process ended with exit status: 1".into());
        state.transition_to(ProcessorState::Restarting);
        state.note_a_helper_process_restart();

        assert_eq!(
            rendered.to_json(),
            serde_json::json!({
                "restart_count": 1,
                "last_exit_reason": "its helper process ended with exit status: 1",
            })
        );
        assert_eq!(
            StateComponent(state).to_json(),
            serde_json::json!("Restarting")
        );
    }

    /// `graph` renders why a processor failed beside its state, and nothing
    /// while it has not failed.
    #[test]
    fn the_failure_reason_renders_only_once_the_processor_has_failed() {
        let state = Arc::new(ObservableProcessorState::new(ProcessorState::Running));
        let rendered = ProcessorFailureReasonComponent(Arc::clone(&state));
        assert_eq!(rendered.to_json(), JsonValue::Null);

        state.fail_with("start() raised: no such file \"lower_third.riv\"");
        state.transition_to_unless_already_failed(ProcessorState::Stopped);

        assert_eq!(
            rendered.to_json(),
            serde_json::json!("start() raised: no such file \"lower_third.riv\"")
        );
    }

    /// `graph` renders what a running processor's `process()` raised beside
    /// its state, keeping the first raise of a run of them, and nothing once a
    /// call runs cleanly again; the state never leaves `Running`.
    #[test]
    fn a_raise_renders_beside_a_running_state_until_process_runs_cleanly() {
        let state = Arc::new(ObservableProcessorState::new(ProcessorState::Running));
        let rendered = ProcessRaisingBecauseComponent(Arc::clone(&state));
        assert_eq!(rendered.to_json(), JsonValue::Null);

        state.note_that_process_raised(|| "process() failed: the first raise".to_string());
        state.note_that_process_raised(|| unreachable!("a raise is already held"));
        assert_eq!(
            rendered.to_json(),
            serde_json::json!("process() failed: the first raise")
        );
        assert_eq!(state.current(), ProcessorState::Running);

        state.note_that_process_ran_cleanly();
        assert_eq!(rendered.to_json(), JsonValue::Null);

        state.note_why_process_is_raising(Some("process() raised KeyError: 'frame'".to_string()));
        assert_eq!(
            rendered.to_json(),
            serde_json::json!("process() raised KeyError: 'frame'")
        );
        state.note_why_process_is_raising(None);
        assert_eq!(rendered.to_json(), JsonValue::Null);
    }

    struct EveryStateChangeHeardOnOneTopic(
        std::sync::mpsc::Sender<(ProcessorUniqueId, ProcessorState, ProcessorState)>,
    );

    impl crate::core::pubsub::EventListener for EveryStateChangeHeardOnOneTopic {
        fn on_event(
            &mut self,
            event: &crate::core::pubsub::Event,
        ) -> crate::core::error::Result<()> {
            if let crate::core::pubsub::Event::OfALoadedStream {
                event:
                    RuntimeEvent::ProcessorStateDidChange {
                        processor_id,
                        old_state,
                        new_state,
                    },
                ..
            } = event
            {
                let _ = self.0.send((processor_id.clone(), *old_state, *new_state));
            }
            Ok(())
        }

        fn on_events_lost(&mut self, _lost_event_count: u64) -> crate::core::error::Result<()> {
            Ok(())
        }
    }

    /// Every write that changes a published state publishes it on its stream's topic, in order,
    /// and a write that leaves the state as it was, or one before publication starts, publishes
    /// nothing.
    ///
    /// Mental-revert: drop the publish from any one write path and its change is missing below.
    #[test]
    fn each_write_that_changes_a_published_state_is_published_on_its_streams_topic() {
        let stream = LoadedStreamIdentity {
            runtime_id: "processor-state-publication-test".to_string(),
            stream_id: "main".to_string(),
            stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(7),
        };
        let (heard, received) = std::sync::mpsc::channel();
        let listener: Arc<Mutex<dyn crate::core::pubsub::EventListener>> =
            Arc::new(Mutex::new(EveryStateChangeHeardOnOneTopic(heard)));
        crate::core::pubsub::PUBSUB
            .subscribe(
                &crate::core::pubsub::topics::loaded_stream(&stream),
                Arc::clone(&listener),
            )
            .expect("subscribe");
        let processor_id = ProcessorUniqueId::from("camera");
        let state = ObservableProcessorState::new(ProcessorState::Pending);
        state.transition_to(ProcessorState::Pending);
        state.publish_each_change_as_a_processor_of(processor_id.clone(), stream);

        state.transition_to(ProcessorState::Idle);
        state.transition_to(ProcessorState::Idle);
        assert!(state.transition_to_only_from(ProcessorState::Idle, ProcessorState::Running));
        assert!(!state.transition_to_only_from(ProcessorState::Idle, ProcessorState::Stopped));
        state.transition_to_unless_already_failed(ProcessorState::Paused);
        state.fail_with("the camera went away");
        state.transition_to_unless_already_failed(ProcessorState::Stopped);

        let every_change: Vec<_> = (0..4)
            .map(|_| {
                received
                    .recv_timeout(UNREACHABLE_DEADLINE)
                    .expect("a change was not published")
            })
            .collect();
        assert_eq!(
            every_change,
            vec![
                (
                    processor_id.clone(),
                    ProcessorState::Pending,
                    ProcessorState::Idle
                ),
                (
                    processor_id.clone(),
                    ProcessorState::Idle,
                    ProcessorState::Running
                ),
                (
                    processor_id.clone(),
                    ProcessorState::Running,
                    ProcessorState::Paused
                ),
                (processor_id, ProcessorState::Paused, ProcessorState::Error),
            ]
        );
        assert!(
            received.recv_timeout(Duration::from_millis(100)).is_err(),
            "a write that changed nothing was published"
        );
    }
}
