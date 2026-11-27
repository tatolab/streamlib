// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Rust node written as one function builds its computed values with its node's limited
//! context, whose GPU escalates in a build for the work a frame is refused.

use serde_json::Value;
use serial_test::serial;

use crate::core::runtime::a_node_creating_a_kernel_in_a_build_for_a_test::{
    the_node_type_creating_a_kernel_in_a_build, what_the_kernel_creating_node_ran,
};
use crate::core::runtime::a_stream_of_one_node_for_a_test::{
    a_stream_of_one_node, once_it_holds, start,
};

/// Mental-revert: a build handed no node context, or one whose GPU refuses to escalate off the
/// frame thread, never builds its kernel, and no frame runs; a frame allowed to escalate records
/// no refusal.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_build_creates_a_kernel_through_its_nodes_context_and_a_frame_is_refused_one() {
    let project_directory = tempfile::tempdir().unwrap();
    let node_type = the_node_type_creating_a_kernel_in_a_build();
    let played = a_stream_of_one_node(project_directory.path(), node_type, Value::Null);
    start(&played.stream);

    let ran = once_it_holds(what_the_kernel_creating_node_ran, |ran| {
        ran.refusal_in_the_latest_frame.is_some()
    });
    assert_eq!(ran.kernels_built, 1, "{ran:?}");
    let refusal = ran.refusal_in_the_latest_frame.expect("a frame ran");
    assert!(
        refusal.contains("`let kernel = computed(move |ctx| … create_compute_kernel(…));`"),
        "{refusal}"
    );
}
