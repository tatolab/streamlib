// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticOnTheWire, DiagnosticPlaceOnTheWire,
    DiagnosticRecoveryOnTheWire, NodeHealthOnTheWire,
};

use crate::core::graph::components::ObservableNodeState;
use crate::core::nodes::NodeState;

/// A scope of one input, `gain`, and one computed value built from it, which fails on a gain
/// below zero and panics on zero; reported to `state`.
fn a_scope_of_one_computed_value_reported_to(
    state: &ObservableNodeState,
    gain: i32,
) -> (
    streamlib_scope::Scope,
    streamlib_scope::FrameView,
    streamlib_scope::Input<i32>,
) {
    let mut declaring = streamlib_scope::ScopeBeingDeclared::new();
    let gain_input = declaring.input(gain);
    declaring.declare(|| {
        streamlib_scope::computed(move || {
            let gain = *gain_input.get();
            assert!(gain != 0, "a gain of zero");
            if gain < 0 {
                return Err(format!("a gain below zero, {gain}"));
            }
            Ok(gain)
        })
    });
    let (scope, view) = declaring.start().expect("the scope starts");
    state.read_the_builds_of_its_computed_values_from(scope.builds());
    (scope, view, gain_input)
}

const SETTLED_WITHIN: Duration = Duration::from_secs(5);

fn the_one_diagnostic_of(
    state: &ObservableNodeState,
    place: &DiagnosticPlaceOnTheWire,
) -> (NodeHealthOnTheWire, DiagnosticOnTheWire) {
    let (health, diagnostics) = state.the_health_of_the_node(place);
    let [diagnostic] = diagnostics.try_into().expect("one diagnostic");
    (health, diagnostic)
}

/// Mental-revert: health read from the node's own reports alone would show a node whose
/// first build failed, and so runs no frame, as healthy.
#[test]
fn a_failed_first_build_shows_the_node_raising_with_its_error_and_the_recovery() {
    let place = DiagnosticPlaceOnTheWire::Stream;
    let state = ObservableNodeState::new(NodeState::Running);
    let (scope, _view, gain) = a_scope_of_one_computed_value_reported_to(&state, -2);
    assert!(scope.wait_until_settled(SETTLED_WITHIN));

    let (health, failed) = the_one_diagnostic_of(&state, &place);
    assert_eq!(health, NodeHealthOnTheWire::Raising);
    assert_eq!(
        failed.kind,
        DiagnosticKindOnTheWire::ComputedValueBuildFailed
    );
    assert_eq!(
        failed.recovery,
        DiagnosticRecoveryOnTheWire::ChangeAnInputTheBuildRead
    );
    assert!(
        failed
            .words
            .starts_with("the build of its computed value `i32` declared at ")
    );
    assert!(
        failed
            .words
            .ends_with("failed: a gain below zero, -2; the node runs no frame until every computed value has one"),
        "{failed:?}"
    );

    scope.write(|writes| writes.write(gain, 3));
    assert!(scope.wait_until_settled(SETTLED_WITHIN));
    assert_eq!(
        state.the_health_of_the_node(&place),
        (NodeHealthOnTheWire::Healthy, Vec::new())
    );
}

/// Mental-revert: a failed rebuild worded as a first build would say the node runs no frame
/// while it runs on its previous value.
#[test]
fn a_failed_rebuild_says_the_node_runs_on_its_previous_value() {
    let place = DiagnosticPlaceOnTheWire::Stream;
    let state = ObservableNodeState::new(NodeState::Running);
    let (scope, _view, gain) = a_scope_of_one_computed_value_reported_to(&state, 1);
    assert!(scope.wait_until_settled(SETTLED_WITHIN));
    scope.write(|writes| writes.write(gain, -5));
    assert!(scope.wait_until_settled(SETTLED_WITHIN));

    let (health, failed) = the_one_diagnostic_of(&state, &place);
    assert_eq!(health, NodeHealthOnTheWire::Raising);
    assert_eq!(
        failed.kind,
        DiagnosticKindOnTheWire::ComputedValueBuildFailed
    );
    assert!(
        failed
            .words
            .ends_with("failed: a gain below zero, -5; the node runs on its previous value"),
        "{failed:?}"
    );
}

/// Mental-revert: a panic reported as a returned error would ask for a new input where only
/// the code recovers.
#[test]
fn a_build_that_panicked_is_its_own_kind_recovered_by_fixing_the_code() {
    let place = DiagnosticPlaceOnTheWire::Stream;
    let state = ObservableNodeState::new(NodeState::Running);
    let (scope, _view, _gain) = a_scope_of_one_computed_value_reported_to(&state, 0);
    assert!(scope.wait_until_settled(SETTLED_WITHIN));

    let (health, panicked) = the_one_diagnostic_of(&state, &place);
    assert_eq!(health, NodeHealthOnTheWire::Raising);
    assert_eq!(
        panicked.kind,
        DiagnosticKindOnTheWire::ComputedValueBuildPanicked
    );
    assert_eq!(panicked.recovery, DiagnosticRecoveryOnTheWire::FixTheCode);
    assert!(
        panicked.words.contains("panicked: a gain of zero;"),
        "{panicked:?}"
    );
}

/// Mental-revert: a pending first build left out of the health would show a node that runs no
/// frame as healthy; a report kept past its scope would show a removed node's builds.
#[test]
fn a_pending_first_build_shows_the_node_not_ready_and_a_closed_scope_shows_nothing() {
    let place = DiagnosticPlaceOnTheWire::Stream;
    let state = ObservableNodeState::new(NodeState::Running);
    let gate = Gate::shut();
    let held = Arc::clone(&gate.0);
    let mut declaring = streamlib_scope::ScopeBeingDeclared::new();
    declaring.declare(|| {
        streamlib_scope::computed(move || {
            Gate::wait_on(&held);
            Ok::<_, String>(1_u8)
        })
    });
    let (scope, view) = declaring.start().expect("the scope starts");
    // Declared after the scope, so it opens before the scope joins its worker on an unwind.
    let gate = gate;
    state.read_the_builds_of_its_computed_values_from(scope.builds());

    let (health, pending) = the_one_diagnostic_of(&state, &place);
    assert_eq!(health, NodeHealthOnTheWire::NotReady);
    assert_eq!(
        pending.kind,
        DiagnosticKindOnTheWire::ComputedValueNotYetBuilt
    );
    assert_eq!(
        pending.recovery,
        DiagnosticRecoveryOnTheWire::WaitForTheNode
    );
    assert!(
        pending.words.ends_with(
            "is building its first value; the node runs no frame until every computed value has one"
        ),
        "{pending:?}"
    );

    drop(gate);
    drop(scope);
    drop(view);
    assert_eq!(
        state.the_health_of_the_node(&place),
        (NodeHealthOnTheWire::Healthy, Vec::new())
    );
}

/// A gate builds wait on until it opens; it opens when dropped, so a failing test never leaves
/// the worker blocked while the scope joins it.
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn shut() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }

    /// Wait until the gate `gate` holds opens.
    fn wait_on(gate: &(Mutex<bool>, Condvar)) {
        let (open, opened) = gate;
        let mut open = open.lock();
        while !*open {
            opened.wait(&mut open);
        }
    }

    fn open(&self) {
        *self.0.0.lock() = true;
        self.0.1.notify_all();
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.open();
    }
}

/// Mental-revert: a report that changed only as a build lands would keep telling the user to
/// change an input they already changed, for as long as the new build runs.
#[test]
fn a_failed_first_build_building_again_from_a_new_input_shows_as_building() {
    let place = DiagnosticPlaceOnTheWire::Stream;
    let state = ObservableNodeState::new(NodeState::Running);
    let gate = Gate::shut();
    let held = Arc::clone(&gate.0);
    let mut declaring = streamlib_scope::ScopeBeingDeclared::new();
    let gain = declaring.input(-1_i32);
    declaring.declare(|| {
        streamlib_scope::computed(move || {
            let gain = *gain.get();
            if gain < 0 {
                return Err(format!("a gain below zero, {gain}"));
            }
            Gate::wait_on(&held);
            Ok(gain)
        })
    });
    let (scope, _view) = declaring.start().expect("the scope starts");
    // Declared after the scope, so it opens before the scope joins its worker on an unwind.
    let gate = gate;
    state.read_the_builds_of_its_computed_values_from(scope.builds());
    assert!(scope.wait_until_settled(SETTLED_WITHIN));
    assert_eq!(
        the_one_diagnostic_of(&state, &place).1.kind,
        DiagnosticKindOnTheWire::ComputedValueBuildFailed
    );

    scope.write(|writes| writes.write(gain, 4));

    let (health, building) = the_one_diagnostic_of(&state, &place);
    assert_eq!(health, NodeHealthOnTheWire::NotReady);
    assert_eq!(
        building.kind,
        DiagnosticKindOnTheWire::ComputedValueNotYetBuilt,
        "{building:?}"
    );
    gate.open();
    assert!(scope.wait_until_settled(SETTLED_WITHIN));
    assert_eq!(
        state.the_health_of_the_node(&place),
        (NodeHealthOnTheWire::Healthy, Vec::new())
    );
}
