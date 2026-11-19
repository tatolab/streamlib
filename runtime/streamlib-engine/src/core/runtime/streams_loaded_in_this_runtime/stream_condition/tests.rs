// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticRecoveryOnTheWire,
};

use super::*;

fn a_node_place(node_name: &str) -> DiagnosticPlaceOnTheWire {
    DiagnosticPlaceOnTheWire::Node {
        node_id: format!("{node_name}-id"),
        node_name: node_name.to_owned(),
    }
}

fn a_diagnostic(
    kind: DiagnosticKindOnTheWire,
    place: DiagnosticPlaceOnTheWire,
) -> DiagnosticOnTheWire {
    DiagnosticOnTheWire::of(kind, place, format!("{kind:?}"))
}

#[test]
fn a_stream_with_no_failure_and_no_node_diagnostic_is_healthy() {
    let condition = the_condition_of_a_stream(
        StreamLoadOnTheWire::Loaded,
        StreamPlaybackOnTheWire::Playing,
        None,
        Vec::new(),
    );
    assert_eq!(
        condition,
        StreamConditionOnTheWire {
            load: StreamLoadOnTheWire::Loaded,
            playback: StreamPlaybackOnTheWire::Playing,
            health: StreamHealthOnTheWire::Healthy,
            diagnostics: Vec::new(),
        }
    );
}

#[test]
fn a_node_diagnostic_alone_degrades_the_stream_and_is_answered_as_it_is() {
    let not_ready = a_diagnostic(
        DiagnosticKindOnTheWire::NodeNotReady,
        a_node_place("camera"),
    );
    let condition = the_condition_of_a_stream(
        StreamLoadOnTheWire::Loaded,
        StreamPlaybackOnTheWire::Playing,
        None,
        vec![not_ready.clone()],
    );
    assert_eq!(condition.health, StreamHealthOnTheWire::Degraded);
    assert_eq!(condition.diagnostics, vec![not_ready]);
}

#[test]
fn a_failure_fails_the_stream_its_diagnostic_first_whatever_its_nodes_say() {
    let failure = StreamFailure::of_the_stream(
        StreamFailureKind::ProjectVenvNotMade,
        "uv refused the pyproject".to_owned(),
    );
    let raising = a_diagnostic(DiagnosticKindOnTheWire::NodeRaising, a_node_place("blur"));
    let condition = the_condition_of_a_stream(
        StreamLoadOnTheWire::Loaded,
        StreamPlaybackOnTheWire::Stopped,
        Some(failure.diagnostic()),
        vec![raising.clone()],
    );
    assert_eq!(condition.health, StreamHealthOnTheWire::Failed);
    assert_eq!(condition.diagnostics, vec![failure.diagnostic(), raising]);
    assert_eq!(
        condition.diagnostics[0].recovery,
        DiagnosticRecoveryOnTheWire::FixTheDependencies
    );
}

#[test]
fn a_failure_a_node_already_carries_is_answered_once_by_that_node() {
    let placeholder = a_node_place("old_filter");
    let failure = StreamFailure {
        kind: StreamFailureKind::NodeTypeNotDescribed,
        place: placeholder.clone(),
        words: "its node `old_filter` is of a type its node code did not describe".to_owned(),
    };
    let of_the_node = a_diagnostic(DiagnosticKindOnTheWire::NodeTypeNotDescribed, placeholder);
    let condition = the_condition_of_a_stream(
        StreamLoadOnTheWire::Loaded,
        StreamPlaybackOnTheWire::Stopped,
        Some(failure.diagnostic()),
        vec![of_the_node.clone()],
    );
    assert_eq!(condition.health, StreamHealthOnTheWire::Failed);
    assert_eq!(condition.diagnostics, vec![of_the_node]);
}

#[test]
fn each_failure_kind_is_answered_as_the_diagnostic_kind_of_its_name() {
    for (kind, on_the_wire) in [
        (
            StreamFailureKind::ProjectVenvNotMade,
            DiagnosticKindOnTheWire::ProjectVenvNotMade,
        ),
        (
            StreamFailureKind::LoadRefused,
            DiagnosticKindOnTheWire::LoadRefused,
        ),
        (
            StreamFailureKind::StartRefused,
            DiagnosticKindOnTheWire::StartRefused,
        ),
        (
            StreamFailureKind::NodeTypeNotDescribed,
            DiagnosticKindOnTheWire::NodeTypeNotDescribed,
        ),
        (
            StreamFailureKind::LinkRefused,
            DiagnosticKindOnTheWire::LinkRefused,
        ),
        (
            StreamFailureKind::KeepsCrashingTheRuntime,
            DiagnosticKindOnTheWire::KeepsCrashingTheRuntime,
        ),
    ] {
        assert_eq!(kind.on_the_wire(), on_the_wire);
    }
}

#[test]
fn an_action_that_loads_starts_or_unloads_a_stream_shows_its_change_and_clears_its_diagnostics() {
    let failed_loaded_and_stopped = the_condition_of_a_stream(
        StreamLoadOnTheWire::Loaded,
        StreamPlaybackOnTheWire::Stopped,
        Some(
            StreamFailure::of_the_stream(StreamFailureKind::StartRefused, "refused".to_owned())
                .diagnostic(),
        ),
        Vec::new(),
    );
    let playing_and_loaded = StreamConditionOnTheWire::healthy(
        StreamLoadOnTheWire::Loaded,
        StreamPlaybackOnTheWire::Playing,
    );
    let not_loaded = StreamConditionOnTheWire::healthy(
        StreamLoadOnTheWire::NotLoaded,
        StreamPlaybackOnTheWire::Stopped,
    );
    for (kind, listed, load, playback) in [
        (
            StreamActionKind::Load,
            &failed_loaded_and_stopped,
            StreamLoadOnTheWire::Loading,
            StreamPlaybackOnTheWire::Stopped,
        ),
        (
            StreamActionKind::Open,
            &not_loaded,
            StreamLoadOnTheWire::Loading,
            StreamPlaybackOnTheWire::Stopped,
        ),
        (
            StreamActionKind::StartPlayback,
            &failed_loaded_and_stopped,
            StreamLoadOnTheWire::Loaded,
            StreamPlaybackOnTheWire::Starting,
        ),
        (
            StreamActionKind::StartPlayback,
            &not_loaded,
            StreamLoadOnTheWire::Loading,
            StreamPlaybackOnTheWire::Starting,
        ),
        (
            StreamActionKind::Close,
            &playing_and_loaded,
            StreamLoadOnTheWire::Unloading,
            StreamPlaybackOnTheWire::Stopping,
        ),
        (
            StreamActionKind::Remove,
            &failed_loaded_and_stopped,
            StreamLoadOnTheWire::Unloading,
            StreamPlaybackOnTheWire::Stopped,
        ),
        (
            StreamActionKind::Delete,
            &not_loaded,
            StreamLoadOnTheWire::NotLoaded,
            StreamPlaybackOnTheWire::Stopped,
        ),
    ] {
        assert_eq!(
            the_condition_while_an_action_works_on_it(kind, listed.clone()),
            StreamConditionOnTheWire::healthy(load, playback),
            "{kind:?} on {listed:?}"
        );
    }
}

#[test]
fn a_stop_under_way_shows_the_stream_stopping_and_keeps_its_health() {
    let degraded = the_condition_of_a_stream(
        StreamLoadOnTheWire::Loaded,
        StreamPlaybackOnTheWire::Playing,
        None,
        vec![a_diagnostic(
            DiagnosticKindOnTheWire::NodeRaising,
            a_node_place("blur"),
        )],
    );
    assert_eq!(
        the_condition_while_an_action_works_on_it(StreamActionKind::Stop, degraded.clone()),
        StreamConditionOnTheWire {
            playback: StreamPlaybackOnTheWire::Stopping,
            ..degraded.clone()
        }
    );
    assert_eq!(
        the_condition_while_an_action_works_on_it(
            StreamActionKind::MakeTheVenvCurrent,
            degraded.clone()
        ),
        degraded
    );
}
