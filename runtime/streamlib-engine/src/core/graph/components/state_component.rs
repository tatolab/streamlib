// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Condvar, Mutex};
use streamlib_runtime_client_contract::local_api_wire_contract::HelperProcessRestartsOnTheWire;

use super::NodeComponentGraphRenders;
use crate::core::error::{Error, PrivacyGatedCaptureDevice};
use crate::core::graph::NodeRunId;
use crate::core::json_schema::GraphNodeComponentsOnTheWire;
use crate::core::nodes::NodeState;
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};

pub struct ObservableNodeState {
    current: Mutex<NodeState>,
    changed: Condvar,
    failure_reason: Mutex<Option<String>>,
    /// The capture device whose access was never asked for, when that is why
    /// `setup()` refused.
    capture_device_access_never_asked_for: Mutex<Option<PrivacyGatedCaptureDevice>>,
    helper_process_restarts: Mutex<HelperProcessRestartRecord>,
    process_raising_because: Mutex<Option<String>>,
    /// Why the node, running, is not ready, in its own words, while it says
    /// so; the runtime keeps its outputs flowing meanwhile.
    not_ready_because: Mutex<Option<String>>,
    state_changes_published_as: Mutex<Option<NodeStateChangesPublishedOnItsStreamsTopic>>,
}

struct NodeStateChangesPublishedOnItsStreamsTopic {
    node_run_id: NodeRunId,
    stream: LoadedStreamIdentity,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelperProcessRestartRecord {
    /// New helper processes started since the stream loaded.
    pub restart_count: u32,
    /// The last death's exit and the end of its standard error.
    pub last_exit_reason: Option<String>,
}

impl ObservableNodeState {
    pub fn new(initial: NodeState) -> Self {
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

    pub fn publish_each_change_as_a_node_of(
        &self,
        node_run_id: NodeRunId,
        stream: LoadedStreamIdentity,
    ) {
        *self.state_changes_published_as.lock() =
            Some(NodeStateChangesPublishedOnItsStreamsTopic {
                node_run_id,
                stream,
            });
    }

    /// Write `new_state` over `current`, which the caller holds locked, wake every waiter, and
    /// publish the change under that lock so the changes publish in the order they were made.
    fn write_the_state_and_publish_the_change(
        &self,
        current: &mut NodeState,
        new_state: NodeState,
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
            published_as
                .stream
                .publish_on_this_streams_topic(RuntimeEvent::NodeStateDidChange {
                    node_run_id: published_as.node_run_id.clone(),
                    old_state,
                    new_state,
                });
        }
    }

    pub fn current(&self) -> NodeState {
        *self.current.lock()
    }

    pub fn transition_to(&self, state: NodeState) {
        self.write_the_state_and_publish_the_change(&mut self.current.lock(), state);
    }

    pub fn fail_with(&self, reason: impl Into<String>) {
        self.fail_with_reason_and_capture_device(reason.into(), None);
    }

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
        self.write_the_state_and_publish_the_change(&mut current, NodeState::Error);
    }

    pub fn failure_reason(&self) -> Option<String> {
        self.failure_reason.lock().clone()
    }

    pub fn capture_device_access_never_asked_for(&self) -> Option<PrivacyGatedCaptureDevice> {
        *self.capture_device_access_never_asked_for.lock()
    }

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

    pub fn transition_to_unless_already_failed(&self, state: NodeState) {
        let mut current = self.current.lock();
        if *current == NodeState::Error {
            return;
        }
        self.write_the_state_and_publish_the_change(&mut current, state);
    }

    pub fn transition_to_only_from(&self, only_from: NodeState, state: NodeState) -> bool {
        let mut current = self.current.lock();
        if *current != only_from {
            return false;
        }
        self.write_the_state_and_publish_the_change(&mut current, state);
        true
    }

    pub fn wait_until_setup_resolved(&self, deadline: Instant) -> NodeState {
        let mut current = self.current.lock();
        while current.is_before_setup_completed() || *current == NodeState::Restarting {
            if self.changed.wait_until(&mut current, deadline).timed_out() {
                break;
            }
        }
        *current
    }
}

pub struct StateComponent(Arc<ObservableNodeState>);

impl StateComponent {
    pub fn new(initial: NodeState) -> Self {
        Self(Arc::new(ObservableNodeState::new(initial)))
    }

    pub fn current(&self) -> NodeState {
        self.0.current()
    }

    pub fn transition_to(&self, state: NodeState) {
        self.0.transition_to(state);
    }

    pub fn fail_with(&self, reason: impl Into<String>) {
        self.0.fail_with(reason);
    }

    pub fn transition_to_only_from(&self, only_from: NodeState, state: NodeState) -> bool {
        self.0.transition_to_only_from(only_from, state)
    }

    pub fn publish_each_change_as_a_node_of(
        &self,
        node_run_id: NodeRunId,
        stream: LoadedStreamIdentity,
    ) {
        self.0.publish_each_change_as_a_node_of(node_run_id, stream);
    }

    pub fn clone_inner(&self) -> Arc<ObservableNodeState> {
        Arc::clone(&self.0)
    }
}

pub struct HelperProcessRestartsComponent(pub Arc<ObservableNodeState>);

impl NodeComponentGraphRenders for HelperProcessRestartsComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        let record = self.0.helper_process_restart_record();
        components.helper_process_restarts = Some(HelperProcessRestartsOnTheWire {
            restart_count: record.restart_count,
            last_exit_reason: record.last_exit_reason,
        });
    }
}

pub struct NodeFailureReasonComponent(pub Arc<ObservableNodeState>);

impl NodeComponentGraphRenders for NodeFailureReasonComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.failed_because = (self.0.current() == NodeState::Error)
            .then(|| self.0.failure_reason())
            .flatten();
    }
}

pub struct ProcessRaisingBecauseComponent(pub Arc<ObservableNodeState>);

impl NodeComponentGraphRenders for ProcessRaisingBecauseComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.process_raising_because = self.0.why_process_is_raising();
    }
}

/// Why a running node is not ready, rendered beside its state as
/// `not_ready_because`: the node's own words while it says so, else absent.
pub struct NodeNotReadyBecauseComponent(pub Arc<ObservableNodeState>);

impl NodeComponentGraphRenders for NodeNotReadyBecauseComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.not_ready_because = self.0.why_the_node_is_not_ready();
    }
}

impl NodeComponentGraphRenders for StateComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.state = Some(self.current().into());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::Value as JsonValue;

    use super::*;

    /// What `graph` renders of `component` under its key, null when it renders none.
    fn the_rendering_of(component: &impl NodeComponentGraphRenders) -> JsonValue {
        let mut components = GraphNodeComponentsOnTheWire::default();
        component.render_onto(&mut components);
        serde_json::to_value(components)
            .unwrap()
            .as_object()
            .and_then(|rendered| rendered.values().next().cloned())
            .unwrap_or(JsonValue::Null)
    }

    /// A deadline far enough out that reaching it means the wait failed to
    /// observe the transition, rather than that the machine was slow.
    const UNREACHABLE_DEADLINE: Duration = Duration::from_secs(30);

    /// The whole point: the wait ends when the transition happens, not when
    /// the deadline expires. A poll loop would pass the state assertion and
    /// fail this one's timing bound.
    #[test]
    fn a_wait_ends_on_the_transition_rather_than_on_the_deadline() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Idle));

        let transitioning = Arc::clone(&state);
        let transition = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            transitioning.transition_to(NodeState::Running);
        });

        let began_waiting = Instant::now();
        let settled = state.wait_until_setup_resolved(began_waiting + UNREACHABLE_DEADLINE);
        let waited = began_waiting.elapsed();
        transition.join().expect("the transition thread panicked");

        assert_eq!(settled, NodeState::Running);
        assert!(
            waited < Duration::from_secs(5),
            "waited {waited:?} for a transition that happened after 50ms — the wait is not \
             riding the change signal"
        );
    }

    #[test]
    fn a_failed_setup_ends_the_wait() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Idle));

        let failing = Arc::clone(&state);
        std::thread::spawn(move || failing.transition_to(NodeState::Error))
            .join()
            .expect("the transition thread panicked");

        assert_eq!(
            state.wait_until_setup_resolved(Instant::now() + UNREACHABLE_DEADLINE),
            NodeState::Error
        );
    }

    /// The missed-wakeup case: the transition already happened, so there is no
    /// notification left to receive and the predicate alone has to end the wait.
    #[test]
    fn a_transition_that_already_happened_does_not_hang_the_wait() {
        let state = ObservableNodeState::new(NodeState::Pending);
        state.transition_to(NodeState::Running);

        assert_eq!(
            state.wait_until_setup_resolved(Instant::now() + UNREACHABLE_DEADLINE),
            NodeState::Running
        );
    }

    #[test]
    fn a_failure_keeps_the_reason_it_failed_for() {
        let state = ObservableNodeState::new(NodeState::Pending);
        assert_eq!(state.failure_reason(), None);

        state.fail_with("setup failed: no permission to create a camera");
        state.transition_to_unless_already_failed(NodeState::Stopped);

        assert_eq!(state.current(), NodeState::Error);
        assert_eq!(
            state.failure_reason().as_deref(),
            Some("setup failed: no permission to create a camera")
        );
        assert_eq!(
            state.wait_until_setup_resolved(Instant::now() + UNREACHABLE_DEADLINE),
            NodeState::Error,
            "a failure with a reason ends the wait like one without"
        );
    }

    #[test]
    fn a_not_ready_reason_is_held_while_the_node_runs_and_forgotten_once_it_stops() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Idle));
        let rendered = NodeNotReadyBecauseComponent(Arc::clone(&state));
        assert_eq!(the_rendering_of(&rendered), JsonValue::Null);

        state.note_why_the_node_is_not_ready(Some("`riv_path` is not set".to_string()));
        state.transition_to(NodeState::Running);
        assert_eq!(
            the_rendering_of(&rendered),
            serde_json::json!("`riv_path` is not set"),
            "a report made from `setup()` survives into the run"
        );
        state.note_why_the_node_is_not_ready(None);

        state.note_why_the_node_is_not_ready(Some("`riv_path` is not set".to_string()));
        assert_eq!(
            the_rendering_of(&rendered),
            serde_json::json!("`riv_path` is not set")
        );

        state.transition_to(NodeState::Paused);
        assert_eq!(
            the_rendering_of(&rendered),
            serde_json::json!("`riv_path` is not set")
        );

        state.note_why_the_node_is_not_ready(None);
        assert_eq!(the_rendering_of(&rendered), JsonValue::Null);

        state.note_why_the_node_is_not_ready(Some("`riv_path` is not set".to_string()));
        state.transition_to(NodeState::Stopped);
        assert_eq!(the_rendering_of(&rendered), JsonValue::Null);

        state.note_why_the_node_is_not_ready(Some("`riv_path` is not set".to_string()));
        assert_eq!(
            the_rendering_of(&rendered),
            JsonValue::Null,
            "a node that does not run holds no not-ready reason"
        );
    }

    /// A setup that refused a capture device nobody asked access for keeps the
    /// device beside its words; any other refusal names no device.
    #[test]
    fn a_setup_refusing_a_capture_device_never_asked_for_keeps_the_device() {
        let waiting_for_the_camera = ObservableNodeState::new(NodeState::Pending);
        waiting_for_the_camera.fail_with_the_setup_refusal(
            &Error::CaptureDeviceAccessNeverAskedFor {
                device: PrivacyGatedCaptureDevice::Camera,
                refusal: "Camera access has not been asked for yet".to_owned(),
            },
        );
        assert_eq!(waiting_for_the_camera.current(), NodeState::Error);
        assert_eq!(
            waiting_for_the_camera.capture_device_access_never_asked_for(),
            Some(PrivacyGatedCaptureDevice::Camera)
        );
        assert_eq!(
            waiting_for_the_camera.failure_reason().as_deref(),
            Some("setup failed: Camera access has not been asked for yet")
        );

        let refused_otherwise = ObservableNodeState::new(NodeState::Pending);
        refused_otherwise
            .fail_with_the_setup_refusal(&Error::Configuration("no such camera".to_owned()));
        assert_eq!(refused_otherwise.current(), NodeState::Error);
        assert_eq!(
            refused_otherwise.capture_device_access_never_asked_for(),
            None
        );
    }

    #[test]
    fn stopping_a_node_does_not_bury_why_it_stopped() {
        let failed = ObservableNodeState::new(NodeState::Running);
        failed.transition_to(NodeState::Error);
        failed.transition_to_unless_already_failed(NodeState::Stopped);
        assert_eq!(failed.current(), NodeState::Error);

        let clean = ObservableNodeState::new(NodeState::Running);
        clean.transition_to_unless_already_failed(NodeState::Stopped);
        assert_eq!(clean.current(), NodeState::Stopped);
    }

    #[test]
    fn a_transition_only_from_a_state_leaves_every_other_state_alone() {
        let running = ObservableNodeState::new(NodeState::Running);
        assert!(running.transition_to_only_from(NodeState::Running, NodeState::Paused));
        assert_eq!(running.current(), NodeState::Paused);

        for kept in [NodeState::Idle, NodeState::Error, NodeState::Stopping] {
            let state = ObservableNodeState::new(kept);
            assert!(!state.transition_to_only_from(NodeState::Running, NodeState::Paused));
            assert_eq!(state.current(), kept);
        }
    }

    #[test]
    fn an_expired_deadline_reports_the_state_it_gave_up_in() {
        let state = ObservableNodeState::new(NodeState::Pending);

        let settled = state.wait_until_setup_resolved(Instant::now() + Duration::from_millis(20));

        assert_eq!(settled, NodeState::Pending);
        assert!(settled.is_before_setup_completed());
    }

    /// Readiness waits through a helper restart for the state the next start
    /// settles on.
    #[test]
    fn a_restarting_node_is_waited_on_until_its_next_start_resolves() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Restarting));
        let restarter = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                state.transition_to(NodeState::Running);
            })
        };

        let settled = state.wait_until_setup_resolved(Instant::now() + Duration::from_secs(5));
        restarter.join().expect("the restarting thread finishes");

        assert_eq!(settled, NodeState::Running);
    }

    #[test]
    fn the_restart_record_renders_its_count_and_the_last_exit_reason() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Running));
        let rendered = HelperProcessRestartsComponent(Arc::clone(&state));
        assert_eq!(
            the_rendering_of(&rendered),
            serde_json::json!({"restart_count": 0, "last_exit_reason": null})
        );

        state.note_a_helper_process_death("its helper process ended with exit status: 1".into());
        state.transition_to(NodeState::Restarting);
        state.note_a_helper_process_restart();

        assert_eq!(
            the_rendering_of(&rendered),
            serde_json::json!({
                "restart_count": 1,
                "last_exit_reason": "its helper process ended with exit status: 1",
            })
        );
        assert_eq!(
            the_rendering_of(&StateComponent(state)),
            serde_json::json!("Restarting")
        );
    }

    #[test]
    fn the_failure_reason_renders_only_once_the_node_has_failed() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Running));
        let rendered = NodeFailureReasonComponent(Arc::clone(&state));
        assert_eq!(the_rendering_of(&rendered), JsonValue::Null);

        state.fail_with("start() raised: no such file \"lower_third.riv\"");
        state.transition_to_unless_already_failed(NodeState::Stopped);

        assert_eq!(
            the_rendering_of(&rendered),
            serde_json::json!("start() raised: no such file \"lower_third.riv\"")
        );
    }

    #[test]
    fn a_raise_renders_beside_a_running_state_until_process_runs_cleanly() {
        let state = Arc::new(ObservableNodeState::new(NodeState::Running));
        let rendered = ProcessRaisingBecauseComponent(Arc::clone(&state));
        assert_eq!(the_rendering_of(&rendered), JsonValue::Null);

        state.note_that_process_raised(|| "process() failed: the first raise".to_string());
        state.note_that_process_raised(|| unreachable!("a raise is already held"));
        assert_eq!(
            the_rendering_of(&rendered),
            serde_json::json!("process() failed: the first raise")
        );
        assert_eq!(state.current(), NodeState::Running);

        state.note_that_process_ran_cleanly();
        assert_eq!(the_rendering_of(&rendered), JsonValue::Null);

        state.note_why_process_is_raising(Some("process() raised KeyError: 'frame'".to_string()));
        assert_eq!(
            the_rendering_of(&rendered),
            serde_json::json!("process() raised KeyError: 'frame'")
        );
        state.note_why_process_is_raising(None);
        assert_eq!(the_rendering_of(&rendered), JsonValue::Null);
    }

    struct EveryStateChangeHeardOnOneTopic(
        std::sync::mpsc::Sender<(NodeRunId, NodeState, NodeState)>,
    );

    impl crate::core::pubsub::EventListener for EveryStateChangeHeardOnOneTopic {
        fn on_event(
            &mut self,
            event: &crate::core::pubsub::Event,
        ) -> crate::core::error::Result<()> {
            if let crate::core::pubsub::Event::OfALoadedStream {
                event:
                    RuntimeEvent::NodeStateDidChange {
                        node_run_id,
                        old_state,
                        new_state,
                    },
                ..
            } = event
            {
                let _ = self.0.send((node_run_id.clone(), *old_state, *new_state));
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
            runtime_id: "node-state-publication-test".to_string(),
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
        let node_run_id = NodeRunId::from("camera");
        let state = ObservableNodeState::new(NodeState::Pending);
        state.transition_to(NodeState::Pending);
        state.publish_each_change_as_a_node_of(node_run_id.clone(), stream);

        state.transition_to(NodeState::Idle);
        state.transition_to(NodeState::Idle);
        assert!(state.transition_to_only_from(NodeState::Idle, NodeState::Running));
        assert!(!state.transition_to_only_from(NodeState::Idle, NodeState::Stopped));
        state.transition_to_unless_already_failed(NodeState::Paused);
        state.fail_with("the camera went away");
        state.transition_to_unless_already_failed(NodeState::Stopped);

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
                (node_run_id.clone(), NodeState::Pending, NodeState::Idle),
                (node_run_id.clone(), NodeState::Idle, NodeState::Running),
                (node_run_id.clone(), NodeState::Running, NodeState::Paused),
                (node_run_id, NodeState::Paused, NodeState::Error),
            ]
        );
        assert!(
            received.recv_timeout(Duration::from_millis(100)).is_err(),
            "a write that changed nothing was published"
        );
    }
}
