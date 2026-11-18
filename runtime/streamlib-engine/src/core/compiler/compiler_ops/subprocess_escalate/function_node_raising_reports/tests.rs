// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use super::*;
use crate::core::graph::{HowItsHelpersRanAFunctionNode, ObservableNodeState};
use crate::core::nodes::NodeState;

fn set_function_node_raising_because(
    what_the_node_reaches: &WhatAHelperPlacedNodeReaches,
    function_node: &str,
    raising_because: Option<&str>,
) {
    let answer = handle_set_function_node_raising_because(
        what_the_node_reaches,
        "r-1".to_string(),
        EscalateRequestSetFunctionNodeRaisingBecause {
            function_node: function_node.to_string(),
            raising_because: raising_because.map(str::to_string),
            request_id: "r-1".to_string(),
        },
    );
    assert!(matches!(answer, EscalateResponse::Ok(_)), "{answer:?}");
}

#[test]
fn a_function_nodes_raise_shows_on_its_own_state_until_the_helper_reports_a_clean_call() {
    let beat_state = Arc::new(ObservableNodeState::new(NodeState::Running));
    let what_the_node_reaches = WhatAHelperPlacedNodeReaches::default();
    what_the_node_reaches
        .how_its_islands_ran_each_function_node
        .report_each_function_node_to([(
            "Pbeat".to_string(),
            Arc::new(HowItsHelpersRanAFunctionNode::showing_its_raises_on(
                Arc::clone(&beat_state),
            )),
        )]);

    set_function_node_raising_because(
        &what_the_node_reaches,
        "Pbeat",
        Some("raised RuntimeError: broke on purpose (nodes/beat.py, line 3)"),
    );
    assert_eq!(
        beat_state.why_process_is_raising().as_deref(),
        Some("raised RuntimeError: broke on purpose (nodes/beat.py, line 3)")
    );

    set_function_node_raising_because(&what_the_node_reaches, "Pbeat", None);
    assert_eq!(beat_state.why_process_is_raising(), None);
}

#[test]
fn a_raise_of_a_node_the_islands_no_longer_call_is_taken_and_passed_over() {
    set_function_node_raising_because(
        &WhatAHelperPlacedNodeReaches::default(),
        "Pgone",
        Some("raised RuntimeError: broke on purpose"),
    );
}
