// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The log level a routine control-plane request speaks at.
//!
//! At the engine's default `info` filter a node's control plane adds
//! nothing to the app's own log. The trace is levelled, not deleted, so
//! `RUST_LOG=tower_http=debug` brings all three records back.

use super::router_surface_tests::control_plane_router_over_a_stub_runtime;
use super::*;
use crate::control_plane_stub_support::CapturedTracingTargets;
use axum::body::Body;
use axum::http::Request;
use serial_test::serial;
use tower::ServiceExt;

fn serve_one_graph_request() {
    let request_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds");
    request_runtime.block_on(async {
        let response = control_plane_router_over_a_stub_runtime()
            .oneshot(
                Request::builder()
                    .uri("/api/graph")
                    .body(Body::empty())
                    .expect("the graph request builds"),
            )
            .await
            .expect("the router answers");
        assert_eq!(response.status(), StatusCode::OK);
    });
}

fn tower_http_trace_targets_under(env_filter_directives: &str) -> Vec<&'static str> {
    CapturedTracingTargets::captured_from_the_second_of_two_runs(
        env_filter_directives,
        serve_one_graph_request,
    )
    .into_iter()
    .filter(|target| target.starts_with("tower_http"))
    .collect()
}

#[test]
#[serial]
fn a_routine_request_says_nothing_at_the_default_info_filter() {
    let targets = tower_http_trace_targets_under("info");
    assert!(
        targets.is_empty(),
        "a request must be silent at the engine's default filter, got: {targets:?}"
    );
}

#[test]
#[serial]
fn the_same_request_traces_all_three_hooks_under_tower_http_debug() {
    let targets = tower_http_trace_targets_under("info,tower_http=debug");
    for hook in [
        "tower_http::trace::make_span",
        "tower_http::trace::on_request",
        "tower_http::trace::on_response",
    ] {
        assert!(
            targets.contains(&hook),
            "asking for the request trace must yield {hook}, got: {targets:?}"
        );
    }
}
