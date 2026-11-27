// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticPlaceOnTheWire, NodeHealthOnTheWire,
};

use super::*;
use crate::core::graph::{BuildsOfItsComputedValues, ObservableNodeState};
use crate::core::nodes::NodeState;

fn report(
    what_the_node_reaches: &WhatAHelperPlacedNodeReaches,
    builds: Vec<ComputedValueBuildsOnTheEscalateWire>,
) {
    let answer = handle_set_computed_value_builds(
        what_the_node_reaches,
        "r-1".to_string(),
        EscalateRequestSetComputedValueBuilds {
            builds,
            request_id: "r-1".to_string(),
        },
    );
    assert!(matches!(answer, EscalateResponse::Ok(_)), "{answer:?}");
}

fn the_scene(
    has_a_value: bool,
    latest_build_failed: Option<ComputedValueBuildFailureOnTheEscalateWire>,
) -> ComputedValueBuildsOnTheEscalateWire {
    ComputedValueBuildsOnTheEscalateWire {
        declared: "`open_the_scene` declared at nodes/rive_source.py:131".to_string(),
        has_a_value,
        building_again: false,
        latest_build_failed,
    }
}

/// Mental-revert: leave the helper's report out of the node's health, and a node function in
/// a helper whose first build raised shows healthy while it publishes nothing.
#[test]
fn a_helper_reporting_a_raised_build_shows_the_node_raising_by_its_own_kind() {
    let what_the_node_reaches = WhatAHelperPlacedNodeReaches::default();
    let state = ObservableNodeState::new(NodeState::Running);
    state.read_the_builds_of_its_computed_values_from(
        BuildsOfItsComputedValues::AsItsHelperReports(Arc::clone(
            &what_the_node_reaches.computed_value_builds_its_helper_reports,
        )),
    );
    let place = DiagnosticPlaceOnTheWire::Stream;
    assert_eq!(
        state.the_health_of_the_node(&place),
        (NodeHealthOnTheWire::Healthy, Vec::new())
    );

    report(
        &what_the_node_reaches,
        vec![the_scene(
            false,
            Some(ComputedValueBuildFailureOnTheEscalateWire::Raised {
                why: "RiveFileNotNamed: `riv_path` is not set".to_string(),
            }),
        )],
    );
    let (health, diagnostics) = state.the_health_of_the_node(&place);
    assert_eq!(health, NodeHealthOnTheWire::Raising);
    let [failed] = diagnostics.try_into().expect("one diagnostic");
    assert_eq!(
        failed.kind,
        DiagnosticKindOnTheWire::ComputedValueBuildFailed
    );
    assert_eq!(
        failed.words,
        "the build of its computed value `open_the_scene` declared at \
         nodes/rive_source.py:131 failed: RiveFileNotNamed: `riv_path` is not set; the node \
         runs no frame until every computed value has one"
    );

    report(&what_the_node_reaches, vec![the_scene(false, None)]);
    let (health, diagnostics) = state.the_health_of_the_node(&place);
    assert_eq!(health, NodeHealthOnTheWire::NotReady);
    assert_eq!(
        diagnostics.iter().map(|d| d.kind).collect::<Vec<_>>(),
        [DiagnosticKindOnTheWire::ComputedValueNotYetBuilt]
    );

    report(&what_the_node_reaches, vec![the_scene(true, None)]);
    assert_eq!(
        state.the_health_of_the_node(&place),
        (NodeHealthOnTheWire::Healthy, Vec::new())
    );
}

/// Mental-revert: map either panicked kind off the wire to a raised build, and the node shows
/// `ComputedValueBuildFailed` with "failed:", unlike the same node run in the host.
#[test]
fn a_helper_reporting_a_panicked_build_words_it_as_the_host_does() {
    let what_the_node_reaches = WhatAHelperPlacedNodeReaches::default();
    let state = ObservableNodeState::new(NodeState::Running);
    state.read_the_builds_of_its_computed_values_from(
        BuildsOfItsComputedValues::AsItsHelperReports(Arc::clone(
            &what_the_node_reaches.computed_value_builds_its_helper_reports,
        )),
    );
    let place = DiagnosticPlaceOnTheWire::Stream;

    for (failed, kind, words) in [
        (
            ComputedValueBuildFailureOnTheEscalateWire::Panicked {
                why: "the device is gone".to_string(),
            },
            DiagnosticKindOnTheWire::ComputedValueBuildPanicked,
            "panicked: the device is gone;",
        ),
        (
            ComputedValueBuildFailureOnTheEscalateWire::ComparisonPanicked {
                why: "__eq__ is gone".to_string(),
            },
            DiagnosticKindOnTheWire::ComputedValueComparisonPanicked,
            "panicked: __eq__ is gone;",
        ),
    ] {
        report(&what_the_node_reaches, vec![the_scene(true, Some(failed))]);
        let (_, diagnostics) = state.the_health_of_the_node(&place);
        let [shown] = diagnostics.try_into().expect("one diagnostic");
        assert_eq!(shown.kind, kind);
        assert!(shown.words.contains(words), "{}", shown.words);
        assert!(!shown.words.contains("it panicked"), "{}", shown.words);
    }
}
